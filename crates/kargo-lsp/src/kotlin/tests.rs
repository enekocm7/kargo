//! Tests of the Kotlin analysis against the built-in outline of the standard library.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use super::actions;
use super::diagnostics::{Severity, Tag};
use super::features::{self, CompletionOptions, Target};
use super::index::{Index, Origin, Platforms, SetKind, SourceSet, Unit, UnitId};
use super::jdk::tests::fake_jdk;
use super::library::{self, SourcesJar, UnitCache, folder_platforms, stub_files};
use super::resolve::{Analyzer, Packages};

struct Fixture {
    lib: Index,
    proj: Index,
    packages: Packages,
    /// The cursor (`$0`) of each file that has one: unit and offset.
    cursors: Vec<(UnitId, usize)>,
    /// The fake JDK's home, kept while the test runs (the JDK writes its files there).
    _jdk: Option<tempfile::TempDir>,
}

/// Project files by path; a path under `tests` is test code, `src-<target>` that target's.
fn fixture(files: &[(&str, &str)]) -> Fixture {
    fixture_with_jdk(files, &[])
}

/// [`fixture`] with the JDK's sources `jdk` (by path, as in `src.zip`) for the libraries.
fn fixture_with_jdk(files: &[(&str, &str)], jdk: &[(&str, &str)]) -> Fixture {
    if jdk.is_empty() {
        return fixture_on(files, None, None, Vec::new());
    }
    let (dir, home) = fake_jdk(jdk);
    let source = super::jdk::Jdk::open(&home, &dir.path().join("cache"), "JDK test").expect("a fake JDK");
    fixture_on(files, Some(Arc::new(source)), Some(dir), Vec::new())
}

/// [`fixture`] with the JDK `jdk` (its cache in `jdk_dir`, which lives as long as the fixture),
/// and more library units `libs`.
fn fixture_on(
    files: &[(&str, &str)],
    jdk: Option<Arc<super::jdk::Jdk>>,
    jdk_dir: Option<tempfile::TempDir>,
    libs: Vec<Arc<Unit>>,
) -> Fixture {
    let mut lib_units: Vec<Arc<Unit>> = stub_files()
        .into_iter()
        .map(|(rel, text)| {
            Arc::new(Unit::new(
                PathBuf::from("/stdlib").join(rel),
                text,
                Origin::Library { name: "stdlib".into(), main: Platforms::ALL, test: Platforms::NONE },
            ))
        })
        .collect();
    lib_units.extend(libs);
    fixture_with(files, lib_units, jdk, jdk_dir)
}

/// [`fixture_on`] with the library files `lib_units` in place of the outline.
fn fixture_with(
    files: &[(&str, &str)],
    lib_units: Vec<Arc<Unit>>,
    jdk: Option<Arc<super::jdk::Jdk>>,
    jdk_dir: Option<tempfile::TempDir>,
) -> Fixture {
    let mut cursors = Vec::new();
    let mut units = Vec::new();
    for (i, (path, text)) in files.iter().enumerate() {
        let first = path.split('/').next().unwrap_or_default();
        let package: usize = if path.starts_with("lib/") { 1 } else { 0 };
        let dir = if package == 1 { path.split('/').nth(1).unwrap_or_default() } else { first };
        let (kind, target) = match dir {
            "tests" => (SetKind::Test, None),
            d if d.starts_with("src-") => (SetKind::Main, Some(d["src-".len()..].to_owned())),
            d if d.starts_with("tests-") => (SetKind::Test, Some(d["tests-".len()..].to_owned())),
            _ => (SetKind::Main, None),
        };
        let origin = if first == "loose" { Origin::Loose } else { Origin::Source(SourceSet { package, kind, target }) };
        let (text, cursor) = match text.find("$0") {
            Some(at) => (text.replacen("$0", "", 1), Some(at)),
            None => (text.to_string(), None),
        };
        if let Some(at) = cursor {
            cursors.push((UnitId { lib: false, unit: i as u32 }, at));
        }
        units.push(Arc::new(Unit::new(PathBuf::from("/project").join(path), text, origin)));
    }
    Fixture {
        lib: Index::new(true, lib_units).with_jdk(jdk),
        proj: Index::new(false, units),
        packages: Packages { visible: vec![vec![0, 1], vec![1]], platforms: vec![Platforms::JVM, Platforms::JVM] },
        cursors,
        _jdk: jdk_dir,
    }
}

impl Fixture {
    fn an(&self) -> Analyzer<'_> {
        Analyzer::new(&self.lib, &self.proj, &self.packages)
    }

    fn cursor(&self) -> (UnitId, usize) {
        self.cursors[0]
    }

    fn labels(&self) -> Vec<String> {
        let (u, offset) = self.cursor();
        let list = features::completion(&self.an(), u, offset, CompletionOptions { snippets: true, ..Default::default() }, 0);
        list["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_owned()).collect()
    }

    fn items(&self) -> Vec<Value> {
        let (u, offset) = self.cursor();
        features::completion(&self.an(), u, offset, CompletionOptions { snippets: true, ..Default::default() }, 0)["items"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// The token at the cursor.
    fn token(&self) -> (UnitId, usize) {
        let (u, offset) = self.cursor();
        let tok = features::token_at(&self.an(), u, offset).expect("a token at the cursor");
        (u, tok)
    }

    fn hover(&self) -> String {
        let (u, tok) = self.token();
        features::hover(&self.an(), u, tok).map(|h| h["contents"]["value"].as_str().unwrap().to_owned()).unwrap_or_default()
    }

    fn signature_help(&self) -> Option<Value> {
        let (u, offset) = self.cursor();
        features::signature_help(&self.an(), u, offset)
    }

    /// Where the identifier at the cursor is declared: file name and the declaration's line.
    fn definition(&self) -> Option<(String, String)> {
        let (u, tok) = self.token();
        let an = self.an();
        let target = features::targets_at(&an, u, tok).into_iter().next()?;
        let Target::Sym(m) = target else { return None };
        let unit = an.unit(m.sym.unit_id());
        let d = an.decl(m.sym);
        let line_start = unit.text[..d.name_span.start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = unit.text[d.name_span.start..].find('\n').map_or(unit.text.len(), |i| d.name_span.start + i);
        Some((unit.path.file_name().unwrap().to_string_lossy().into_owned(), unit.text[line_start..line_end].trim().to_owned()))
    }

    /// The edits of renaming what is at the cursor: file name and the edited lines.
    fn rename(&self, to: &str) -> Vec<(String, String)> {
        let (u, tok) = self.token();
        let an = self.an();
        let sym = features::targets_at(&an, u, tok).into_iter().find_map(|t| t.sym()).expect("a declaration");
        let group = features::rename_group(&an, sym);
        let mut out: Vec<(String, String)> = Vec::new();
        let mut edits: std::collections::BTreeMap<(String, usize), String> = Default::default();
        let mut by_unit: std::collections::HashMap<UnitId, Vec<std::ops::Range<usize>>> = Default::default();
        for (unit, range) in features::references(&an, &group, true) {
            by_unit.entry(unit).or_default().push(range);
        }
        for (unit, mut ranges) in by_unit {
            let u = an.unit(unit);
            let mut text = u.text.clone();
            ranges.sort_by_key(|r| std::cmp::Reverse(r.start));
            for r in &ranges {
                text.replace_range(r.clone(), to);
            }
            let name = u.path.file_name().unwrap().to_string_lossy().into_owned();
            for (n, line) in text.lines().enumerate() {
                if line.contains(to) {
                    edits.insert((name.clone(), n), line.trim().to_owned());
                }
            }
        }
        for ((file, _), line) in edits {
            out.push((file, line));
        }
        out
    }
}

fn has(labels: &[String], name: &str) -> bool {
    labels.iter().any(|l| l == name)
}

#[test]
fn completes_members_of_strings_and_collections() {
    let f = fixture(&[("src/Main.kt", "fun main() {\n    val s = \"abc\"\n    s.$0\n}\n")]);
    let labels = f.labels();
    for name in ["length", "uppercase", "trim", "plus", "let", "apply", "toString", "isBlank", "split"] {
        assert!(has(&labels, name), "{name} in {labels:?}");
    }
    assert!(!has(&labels, "forEach"), "{labels:?}");
    // Only the postfix template, not the global function.
    assert!(!f.items().iter().any(|i| i["label"] == "println" && i["kind"] != 15), "{labels:?}");

    let f = fixture(&[("src/Main.kt", "fun main() {\n    listOf(1, 2).$0\n}\n")]);
    let labels = f.labels();
    for name in ["map", "forEach", "size", "first", "joinToString", "sum", "get", "isEmpty"] {
        assert!(has(&labels, name), "{name} in {labels:?}");
    }
    assert!(!has(&labels, "uppercase"), "{labels:?}");
}

#[test]
fn infers_lambda_parameters_and_receivers() {
    let f = fixture(&[("src/Main.kt", "fun main() {\n    listOf(\"a\").forEach { it.$0 }\n}\n")]);
    assert!(has(&f.labels(), "uppercase"), "{:?}", f.labels());

    let f = fixture(&[("src/Main.kt", "fun main() {\n    listOf(\"a\").map { it.length }.first().$0\n}\n")]);
    let labels = f.labels();
    assert!(has(&labels, "toLong") && has(&labels, "shl"), "{labels:?}");

    let f = fixture(&[("src/Main.kt", "fun main() {\n    val text = buildString { app$0 }\n}\n")]);
    assert!(has(&f.labels(), "append"), "{:?}", f.labels());

    let f = fixture(&[("src/Main.kt", "fun main() {\n    mapOf(\"a\" to 1).forEach { (key, value) -> key.$0 }\n}\n")]);
    assert!(has(&f.labels(), "uppercase"), "{:?}", f.labels());

    let f = fixture(&[("src/Main.kt", "fun main() {\n    for ((k, v) in mapOf(\"a\" to 1)) {\n        v.$0\n    }\n}\n")]);
    assert!(has(&f.labels(), "shl"), "{:?}", f.labels());

    let f = fixture(&[("src/Main.kt", "fun main() {\n    for (i in 0 until 10) {\n        i.$0\n    }\n}\n")]);
    assert!(has(&f.labels(), "shl"), "{:?}", f.labels());

    let f = fixture(&[("src/Main.kt", "class A {\n    val label get() = \"a\"\n}\nfun main() {\n    A().label.$0\n}\n")]);
    assert!(has(&f.labels(), "length"), "a getter's type: {:?}", f.labels());

    let f = fixture(&[("src/Main.kt", "class Box<T>(val value: T)\nfun main() {\n    Box(\"a\").value.$0\n}\n")]);
    assert!(has(&f.labels(), "length"), "a class's type argument from its constructor: {:?}", f.labels());

    let f = fixture(&[("src/Main.kt", "class Greeter {\n    fun greet() = \"hi\"\n    fun twice() = this.$0\n}\n")]);
    assert!(has(&f.labels(), "greet"), "{:?}", f.labels());

    let f = fixture(&[("src/Main.kt", "fun main() {\n    with(StringBuilder()) { app$0 }\n}\n")]);
    assert!(has(&f.labels(), "append"), "with's receiver: {:?}", f.labels());

    let f = fixture(&[("src/Main.kt", "fun main() {\n    val n = if (true) \"a\" else \"b\"\n    n.$0\n}\n")]);
    assert!(has(&f.labels(), "length"), "an if expression: {:?}", f.labels());

    let f = fixture(&[("src/Main.kt", "fun main() {\n    \"x\".let { s -> s.$0 }\n}\n")]);
    assert!(has(&f.labels(), "length"), "{:?}", f.labels());
}

#[test]
fn completes_names_in_scope() {
    let src = r#"package app

class Greeter(private val name: String) {
    private val prefix = "Hello"
    fun greet(): String {
        val count = 1
        return gr$0
    }
    fun other() {}
}
"#;
    let f = fixture(&[("src/Greeter.kt", src)]);
    let labels = f.labels();
    assert!(has(&labels, "greet"), "{labels:?}");
    assert!(has(&labels, "Greeter"), "{labels:?}");
    let f = fixture(&[("src/Greeter.kt", &src.replace("gr$0", "$0"))]);
    let labels = f.labels();
    for name in ["count", "name", "prefix", "greet", "other", "println", "listOf", "Greeter", "String", "return", "if"] {
        assert!(has(&labels, name), "{name} in {labels:?}");
    }
    // Locals come first.
    let items = f.items();
    let count = items.iter().find(|i| i["label"] == "count").unwrap();
    let println = items.iter().find(|i| i["label"] == "println").unwrap();
    assert!(count["sortText"].as_str() < println["sortText"].as_str());
}

#[test]
fn completion_inserts_calls_and_imports() {
    let f = fixture(&[
        ("src/util/Strings.kt", "package app.util\n\nfun shout(text: String): String = text.uppercase()\nclass Helper\n"),
        ("src/Main.kt", "package app\n\nfun main() {\n    sho$0\n}\n"),
    ]);
    let items = f.items();
    let shout = items.iter().find(|i| i["label"] == "shout").expect("shout is offered");
    assert_eq!(shout["textEdit"]["newText"], "shout($0)");
    assert_eq!(shout["insertTextFormat"], 2);
    let import = &shout["additionalTextEdits"][0];
    assert_eq!(import["newText"], "\n\nimport app.util.shout");
    assert_eq!(import["range"]["start"]["line"], 0);
    assert!(shout["detail"].as_str().unwrap().starts_with("app.util · "), "the package leads the detail: {shout}");

    let f = fixture(&[("src/Main.kt", "fun main() {\n    printl$0\n}\n")]);
    let println = f.items().into_iter().find(|i| i["label"] == "println").unwrap();
    assert!(println["additionalTextEdits"].is_null(), "println is imported by default");
    assert!(println["detail"].as_str().unwrap().contains("fun println("), "{println}");
}

#[test]
fn completion_inserts_scaffolds_for_keywords_with_a_condition() {
    let keyword = |items: &[Value], name: &str| items.iter().find(|i| i["label"] == name && i["kind"] == 14).unwrap().clone();

    let f = fixture(&[("src/Main.kt", "fun main() {\n    whi$0\n}\n")]);
    let items = f.items();
    let while_ = keyword(&items, "while");
    assert_eq!(while_["textEdit"]["newText"], "while ($0)");
    assert_eq!(while_["insertTextFormat"], 2);

    let f = fixture(&[("src/Main.kt", "fun main() {\n    i$0\n}\n")]);
    let items = f.items();
    assert_eq!(keyword(&items, "if")["textEdit"]["newText"], "if ($0)");

    let f = fixture(&[("src/Main.kt", "fun main() {\n    wh$0\n}\n")]);
    let items = f.items();
    assert_eq!(keyword(&items, "when")["textEdit"]["newText"], "when ($0) {}");

    // Keywords without a condition stay plain text.
    let f = fixture(&[("src/Main.kt", "fun main() {\n    va$0\n}\n")]);
    let items = f.items();
    let val = keyword(&items, "val");
    assert_eq!(val["textEdit"]["newText"], "val");
    assert_eq!(val["insertTextFormat"], 1);

    // Without snippet support, and when a parenthesis already follows, the plain keyword is inserted.
    let f = fixture(&[("src/Main.kt", "fun main() {\n    whi$0\n}\n")]);
    let (u, offset) = f.cursor();
    let list = features::completion(&f.an(), u, offset, CompletionOptions { snippets: false, ..Default::default() }, 0);
    let plain = keyword(list["items"].as_array().unwrap(), "while");
    assert_eq!(plain["textEdit"]["newText"], "while");
    assert_eq!(plain["insertTextFormat"], 1);

    let f = fixture(&[("src/Main.kt", "fun main() {\n    whi$0(true)\n}\n")]);
    let items = f.items();
    assert_eq!(keyword(&items, "while")["textEdit"]["newText"], "while");
}

/// The label of parameter `k` of signature `s`, read through its offsets.
fn parameter_label(help: &Value, s: usize, k: usize) -> String {
    let sig = &help["signatures"][s];
    let range = &sig["parameters"][k]["label"];
    let (a, b) = (range[0].as_u64().unwrap() as usize, range[1].as_u64().unwrap() as usize);
    let units: Vec<u16> = sig["label"].as_str().unwrap().encode_utf16().collect();
    String::from_utf16(&units[a..b]).unwrap()
}

#[test]
fn signature_help_follows_the_active_parameter() {
    let src = "/** Adds.\n * @param a the first */\nfun add(a: Int, b: Int = 0) = a + b\nfun main() {\n    add(1, $0\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    let help = f.signature_help().expect("help inside the call");
    assert_eq!(help["signatures"].as_array().unwrap().len(), 1);
    assert_eq!(help["signatures"][0]["label"], "fun add(a: Int, b: Int = …): Int");
    assert_eq!(help["activeParameter"], 1);
    assert_eq!(parameter_label(&help, 0, 0), "a: Int");
    assert_eq!(parameter_label(&help, 0, 1), "b: Int = …");
    assert_eq!(help["signatures"][0]["parameters"][0]["documentation"]["value"], "the first");

    let f = fixture(&[("src/Main.kt", "fun add(a: Int, b: Int = 0) = a + b\nfun main() {\n    add($0\n}\n")]);
    assert_eq!(f.signature_help().unwrap()["activeParameter"], 0);

    // Commas inside nested calls and a named argument.
    let f = fixture(&[("src/Main.kt", "fun add(a: Int, b: Int = 0) = a + b\nfun main() {\n    add(add(1, 2), $0)\n}\n")]);
    assert_eq!(f.signature_help().unwrap()["activeParameter"], 1);
    let f = fixture(&[("src/Main.kt", "fun add(a: Int, b: Int = 0) = a + b\nfun main() {\n    add(b = $0)\n}\n")]);
    assert_eq!(f.signature_help().unwrap()["activeParameter"], 1);

    // Outside any call there is no help.
    let f = fixture(&[("src/Main.kt", "fun add(a: Int) = a\nfun main() {\n    add(1)$0\n}\n")]);
    assert!(f.signature_help().is_none());
}

#[test]
fn signature_help_lists_overloads_best_fit_first() {
    let src = "fun show(a: Int) = a\nfun show(a: String, b: Int) = b\nfun main() {\n    show(\"x\", $0\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    let help = f.signature_help().unwrap();
    let sigs = help["signatures"].as_array().unwrap();
    assert_eq!(sigs.len(), 2, "{help}");
    assert!(sigs[0]["label"].as_str().unwrap().contains("a: String, b: Int"), "{help}");
    assert_eq!(help["activeSignature"], 0);
    assert_eq!(help["activeParameter"], 1);

    let f = fixture(&[("src/Main.kt", "fun main() {\n    listOf($0\n}\n")]);
    let help = f.signature_help().unwrap();
    assert!(help["signatures"].as_array().unwrap().len() >= 1, "{help}");
}

#[test]
fn hovers_with_signatures_and_documentation() {
    let f = fixture(&[("src/Main.kt", "fun main() {\n    print$0ln(\"hi\")\n}\n")]);
    let hover = f.hover();
    assert!(hover.contains("fun println(message: Any?)"), "{hover}");
    assert!(hover.contains("standard output"), "{hover}");
    assert!(hover.contains("kotlin.io"), "{hover}");

    let f = fixture(&[(
        "src/Main.kt",
        "/** Adds [a] and [b].\n * @param a the first */\nfun add(a: Int, b: Int = 0) = a + b\nfun main() {\n    val x = ad$0d(1)\n}\n",
    )]);
    let hover = f.hover();
    assert!(hover.contains("fun add(a: Int, b: Int = …): Int"), "{hover}");
    assert!(hover.contains("Adds `a` and `b`."), "{hover}");
    assert!(hover.contains("- `a` the first"), "{hover}");

    let f = fixture(&[("src/Main.kt", "fun main() {\n    val names = listOf(\"a\")\n    nam$0es.size\n}\n")]);
    assert!(f.hover().contains("val names: List<String>"), "{}", f.hover());
}

#[test]
fn goes_to_definitions() {
    let f = fixture(&[
        ("src/model/User.kt", "package app.model\n\ndata class User(val name: String) {\n    fun greet() = \"hi $name\"\n}\n"),
        ("src/Main.kt", "package app\n\nimport app.model.User\n\nfun main() {\n    val user = User(\"a\")\n    user.gre$0et()\n}\n"),
    ]);
    assert_eq!(f.definition(), Some(("User.kt".into(), "fun greet() = \"hi $name\"".into())));

    let f = fixture(&[
        ("src/model/User.kt", "package app.model\n\ndata class User(val name: String)\n"),
        ("src/Main.kt", "package app\n\nimport app.model.User\n\nfun main() {\n    val user = Us$0er(\"a\")\n}\n"),
    ]);
    assert_eq!(f.definition(), Some(("User.kt".into(), "data class User(val name: String)".into())));

    let f = fixture(&[("src/Main.kt", "fun main() {\n    val total = 1\n    println(tot$0al)\n}\n")]);
    assert_eq!(f.definition(), Some(("Main.kt".into(), "val total = 1".into())));

    let f = fixture(&[("src/Main.kt", "fun main() {\n    printl$0n(1)\n}\n")]);
    assert_eq!(f.definition().map(|d| d.0), Some("Console.kt".into()));

    // Same package, other file; a named argument.
    let f = fixture(&[
        ("src/a/Config.kt", "package app\n\nfun configure(verbose: Boolean = false) {}\n"),
        ("src/Main.kt", "package app\n\nfun main() {\n    configure(verb$0ose = true)\n}\n"),
    ]);
    assert_eq!(f.definition(), Some(("Config.kt".into(), "fun configure(verbose: Boolean = false) {}".into())));
}

/// The declarations of `pick`, and the return type of `val r = ...` at the second cursor.
fn overloads(decls: &str, call: &str) -> Fixture {
    fixture(&[("src/Main.kt", &format!("open class Base\nclass Derived : Base()\n{decls}\nfun main() {{\n    {call}\n}}\n"))])
}

#[test]
fn picks_overloads_by_subtype() {
    let decls = "fun log(message: String) {}\nfun log(error: Throwable) {}\nfun log(base: Base) {}\n";
    let f = overloads(decls, "lo$0g(IllegalStateException(\"x\"))");
    assert_eq!(f.definition().map(|d| d.1), Some("fun log(error: Throwable) {}".into()));
    let f = overloads(decls, "lo$0g(\"x\")");
    assert_eq!(f.definition().map(|d| d.1), Some("fun log(message: String) {}".into()));
    // The most specific applicable overload wins.
    let decls = "fun take(a: Any) {}\nfun take(b: Base) {}\n";
    let f = overloads(decls, "ta$0ke(Derived())");
    assert_eq!(f.definition().map(|d| d.1), Some("fun take(b: Base) {}".into()));
    // Renaming one overload leaves the calls of the other.
    let f = overloads("fun log(message: String) {}\nfun log(base: Base) {}\n", "lo$0g(Derived())\n    log(\"x\")");
    let edits = f.rename("note");
    assert!(edits.iter().any(|(_, l)| l.contains("fun note(base: Base)")), "{edits:?}");
    assert!(edits.iter().any(|(_, l)| l.contains("note(Derived())")), "{edits:?}");
    assert!(!edits.iter().any(|(_, l)| l.contains("note(\"x\")") || l.contains("note(message")), "{edits:?}");
}

#[test]
fn picks_overloads_by_nullability_and_function_types() {
    let decls = "fun len(s: String): Int = 0\nfun len(s: String?): Long = 0\n";
    let f = overloads(decls, "val x: String? = null\n    val r = len(x)\n    r$0");
    assert!(f.hover().contains("Long"), "{}", f.hover());
    let f = overloads(decls, "val r = len(\"a\")\n    r$0");
    assert!(f.hover().contains("Int"), "{}", f.hover());

    let decls = "fun run(block: () -> String): Int = 0\nfun run(block: (Int) -> String): Long = 0\n";
    let f = overloads(decls, "val r = run { a -> \"x\" }\n    r$0");
    assert!(f.hover().contains("Long"), "{}", f.hover());
    let f = overloads(decls, "val r = run { \"x\" }\n    r$0");
    assert!(f.hover().contains("Int"), "{}", f.hover());
}

#[test]
fn picks_overloads_by_type_arguments_and_their_variance() {
    let decls = "class Source<out T>(val value: T)\nclass Box<T>(val value: T)\nfun take(list: List<String>): Int = 0\nfun take(list: List<Int>): Long = 0\n";
    let f = overloads(decls, "val r = take(listOf(1))\n    r$0");
    assert!(f.hover().contains("Long"), "{}", f.hover());
    let f = overloads(decls, "val r = take(listOf(\"a\"))\n    r$0");
    assert!(f.hover().contains("Int"), "{}", f.hover());

    // `out`: a `Source<String>` is a `Source<CharSequence>`, but not a `Source<Int>`.
    let decls = "class Source<out T>(val value: T)\nfun give(s: Source<CharSequence>): Int = 0\nfun give(s: Source<Int>): Long = 0\n";
    let f = overloads(decls, "val r = give(Source(\"a\"))\n    r$0");
    assert!(f.hover().contains("Int"), "{}", f.hover());

    // Invariant: a `Box<String>` is not a `Box<CharSequence>`.
    let decls = "class Box<T>(val value: T)\nfun keep(b: Box<CharSequence>): Int = 0\nfun keep(b: Box<String>): Long = 0\n";
    let f = overloads(decls, "val r = keep(Box(\"a\"))\n    r$0");
    assert!(f.hover().contains("Long"), "{}", f.hover());
    let f = overloads(decls, "val r = keep(Box<CharSequence>(\"a\"))\n    r$0");
    assert!(f.hover().contains("Int"), "{}", f.hover());
}

#[test]
fn keeps_every_overload_when_the_arguments_are_unknown() {
    let f = overloads("fun log(message: String) {}\nfun log(base: Base) {}\n", "lo$0g(unknown)");
    let (u, tok) = f.token();
    let an = f.an();
    assert_eq!(features::targets_at(&an, u, tok).len(), 2);
}

#[test]
fn respects_source_sets() {
    let f = fixture(&[
        ("tests/MainTest.kt", "package app\n\nfun testHelper() {}\n"),
        ("src/Main.kt", "package app\n\nfun main() {\n    testHel$0\n}\n"),
    ]);
    assert!(!has(&f.labels(), "testHelper"), "main code doesn't see tests");
    let f = fixture(&[
        ("src/Main.kt", "package app\n\nfun helper() {}\n"),
        ("tests/MainTest.kt", "package app\n\nfun test() {\n    help$0\n}\n"),
    ]);
    assert!(has(&f.labels(), "helper"), "tests see main code");
    let f = fixture(&[
        ("src-jvm/Platform.kt", "package app\n\nfun jvmOnly() {}\n"),
        ("src/Main.kt", "package app\n\nfun main() {\n    jvmOn$0\n}\n"),
    ]);
    assert!(!has(&f.labels(), "jvmOnly"), "common code doesn't see the JVM's");
    let f = fixture(&[
        ("src/Common.kt", "package app\n\nfun common() {}\n"),
        ("src-jvm/Platform.kt", "package app\n\nfun jvm() {\n    comm$0\n}\n"),
    ]);
    assert!(has(&f.labels(), "common"), "the JVM's code sees common code");
    // A path dependency's main code is visible, not the other way around.
    let f = fixture(&[
        ("lib/src/Lib.kt", "package lib\n\nfun libFunction() {}\n"),
        ("src/Main.kt", "package app\n\nimport lib.libFunction\n\nfun main() {\n    libFunc$0tion()\n}\n"),
    ]);
    assert_eq!(f.definition().map(|d| d.0), Some("Lib.kt".into()));
}

#[test]
fn renames_across_files() {
    let f = fixture(&[
        ("src/model/User.kt", "package app.model\n\nclass User(val name: String)\n"),
        (
            "src/Main.kt",
            "package app\n\nimport app.model.User\n\nfun main() {\n    val user: User = Us$0er(\"a\")\n    println(user.name)\n}\n",
        ),
    ]);
    let edits = f.rename("Person");
    assert_eq!(
        edits,
        [
            ("Main.kt".into(), "import app.model.Person".into()),
            ("Main.kt".into(), "val user: Person = Person(\"a\")".into()),
            ("User.kt".into(), "class Person(val name: String)".into()),
        ]
    );

    let src = "abstract class Shape {\n    abstract fun area(): Double\n}\nclass Square(val side: Double) : Shape() {\n    override fun ar$0ea() = side * side\n}\nfun total(shapes: List<Shape>) = shapes.map { it.area() }\n";
    let f = fixture(&[("src/Shapes.kt", src)]);
    let edits = f.rename("size");
    assert_eq!(edits.len(), 3, "{edits:?}");

    let f = fixture(&[("src/Main.kt", "fun main() {\n    val count = 1\n    val other = coun$0t + 1\n    println(\"$count\")\n}\n")]);
    let edits = f.rename("total");
    assert_eq!(
        edits,
        [
            ("Main.kt".into(), "val total = 1".into()),
            ("Main.kt".into(), "val other = total + 1".into()),
            ("Main.kt".into(), "println(\"$total\")".into())
        ]
    );
}

#[test]
fn renames_parameters_with_their_named_arguments() {
    let f = fixture(&[
        ("src/Greet.kt", "package app\n\nfun greet(na$0me: String) = \"hi $name\"\n"),
        ("src/Main.kt", "package app\n\nfun main() {\n    greet(name = \"a\")\n    val name = 1\n}\n"),
    ]);
    assert_eq!(
        f.rename("who"),
        [("Greet.kt".into(), "fun greet(who: String) = \"hi $who\"".into()), ("Main.kt".into(), "greet(who = \"a\")".into()),]
    );
}

#[test]
fn renames_expect_and_actual_together() {
    let f = fixture(&[
        ("src/Platform.kt", "package app\n\nexpect fun platformName(): String\n\nfun describe() = platform$0Name()\n"),
        ("src-jvm/Platform.jvm.kt", "package app\n\nactual fun platformName(): String = \"JVM\"\n"),
    ]);
    let edits = f.rename("hostName");
    let files: Vec<&str> = edits.iter().map(|(f, _)| f.as_str()).collect();
    assert!(files.contains(&"Platform.jvm.kt"), "{edits:?}");
    assert_eq!(edits.len(), 3, "{edits:?}");
}

#[test]
fn completes_imports_and_qualified_names() {
    let f = fixture(&[("src/Main.kt", "import kotlin.collections.$0\n")]);
    let labels = f.labels();
    assert!(has(&labels, "List") && has(&labels, "listOf"), "{labels:?}");
    let f = fixture(&[("src/Main.kt", "fun main() {\n    kotlin.collections.$0\n}\n")]);
    assert!(has(&f.labels(), "mutableListOf"), "{:?}", f.labels());
    let f = fixture(&[("src/Main.kt", "fun main() {\n    Int.$0\n}\n")]);
    assert!(has(&f.labels(), "MAX_VALUE"), "{:?}", f.labels());
}

#[test]
fn completes_enum_entries_and_companions() {
    let src = "enum class Color { RED, GREEN; fun lower() = name.lowercase() }\nclass Box { companion object { fun empty() = Box() } }\nfun main() {\n    Color.$0\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    let labels = f.labels();
    assert!(has(&labels, "RED") && has(&labels, "GREEN"), "{labels:?}");
    let f = fixture(&[("src/Main.kt", &src.replace("Color.$0", "Color.RED.$0"))]);
    let labels = f.labels();
    assert!(has(&labels, "lower") && has(&labels, "name") && has(&labels, "ordinal"), "{labels:?}");
    let f = fixture(&[("src/Main.kt", &src.replace("Color.$0", "Box.$0"))]);
    assert!(has(&f.labels(), "empty"), "{:?}", f.labels());
    let f = fixture(&[("src/Main.kt", &src.replace("Color.$0", "Box.empty().$0"))]);
    assert!(has(&f.labels(), "toString"), "{:?}", f.labels());
}

#[test]
fn nothing_in_comments_and_strings() {
    let f = fixture(&[("src/Main.kt", "// pri$0\n")]);
    assert!(f.labels().is_empty());
    let f = fixture(&[("src/Main.kt", "val s = \"pri$0\"\n")]);
    assert!(f.labels().is_empty());
    let f = fixture(&[("src/Main.kt", "val s = \"${pri$0}\"\n")]);
    assert!(has(&f.labels(), "print"));
}

/// [`fixture`] with the library files `libs`, each with the platforms it is on for main code and for
/// test code only, and the platforms of the packages.
fn platform_fixture(files: &[(&str, &str)], libs: &[(&str, &str, Platforms, Platforms)], platforms: Vec<Platforms>) -> Fixture {
    let lib_units: Vec<Arc<Unit>> = libs
        .iter()
        .map(|&(path, text, main, test)| {
            Arc::new(Unit::new(PathBuf::from("/lib").join(path), text.to_owned(), Origin::Library { name: "lib".into(), main, test }))
        })
        .collect();
    let mut f = fixture_with(files, lib_units, None, None);
    f.packages.platforms = platforms;
    f
}

/// A library with a declaration on each platform, and one for test code only.
fn demo_libs() -> Vec<(&'static str, &'static str, Platforms, Platforms)> {
    vec![
        ("kotlin/Common.kt", "package kotlin.demo\n\nfun shared() = 1\n", Platforms::ALL, Platforms::NONE),
        ("jvmMain/kotlin/demo/Jvm.kt", "package kotlin.demo\n\nfun jvmOnly() = 1\n", Platforms::JVM, Platforms::NONE),
        ("wasmJsMain/kotlin/demo/Wasm.kt", "package kotlin.demo\n\nfun wasmOnly() = 1\n", Platforms::WASM, Platforms::NONE),
        ("kotlin/test/Assert.kt", "package kotlin.demo\n\nfun testOnly() = 1\n", Platforms::NONE, Platforms::ALL),
    ]
}

/// Whether code of the project file at position `from` sees a top-level declaration named `fq`.
fn sees(f: &Fixture, from: usize, fq: &str) -> bool {
    let an = f.an();
    let syms = an.by_fq(fq);
    assert!(!syms.is_empty(), "{fq} is indexed");
    let from = UnitId { lib: false, unit: from as u32 };
    syms.iter().any(|&s| an.visible(from, s))
}

#[test]
fn a_multi_target_package_sees_only_what_every_target_has_in_common_code() {
    let f = platform_fixture(
        &[
            ("src/Common.kt", "package app\n"),
            ("src-jvm/Jvm.kt", "package app\n"),
            ("src-wasm/Wasm.kt", "package app\n"),
            ("tests/Test.kt", "package app\n"),
        ],
        &demo_libs(),
        vec![Platforms::JVM.union(Platforms::WASM), Platforms::NONE],
    );
    // Common code compiles for JVM and Wasm alike, so it sees the libraries of both, and not the
    // test-only ones.
    assert!(sees(&f, 0, "kotlin.demo.shared"));
    assert!(!sees(&f, 0, "kotlin.demo.jvmOnly"));
    assert!(!sees(&f, 0, "kotlin.demo.wasmOnly"));
    assert!(!sees(&f, 0, "kotlin.demo.testOnly"));
    // Each target sees its own platform's libraries, and the common ones.
    assert!(sees(&f, 1, "kotlin.demo.shared") && sees(&f, 1, "kotlin.demo.jvmOnly"));
    assert!(!sees(&f, 1, "kotlin.demo.wasmOnly"));
    assert!(sees(&f, 2, "kotlin.demo.shared") && sees(&f, 2, "kotlin.demo.wasmOnly"));
    assert!(!sees(&f, 2, "kotlin.demo.jvmOnly"));
    // Test code sees the test-only libraries, and still only what every target has.
    assert!(sees(&f, 3, "kotlin.demo.shared") && sees(&f, 3, "kotlin.demo.testOnly"));
    assert!(!sees(&f, 3, "kotlin.demo.jvmOnly"));
}

#[test]
fn a_jvm_only_package_sees_jvm_libraries_in_common_code() {
    // A file outside the source roots, open in the editor, sees every platform's libraries.
    let f =
        platform_fixture(&[("src/Common.kt", "package app\n"), ("loose/Scratch.kt", "package app\n")], &demo_libs(), vec![Platforms::JVM]);
    assert!(sees(&f, 0, "kotlin.demo.jvmOnly") && sees(&f, 0, "kotlin.demo.shared"));
    assert!(!sees(&f, 0, "kotlin.demo.wasmOnly"));
    assert!(sees(&f, 1, "kotlin.demo.wasmOnly") && sees(&f, 1, "kotlin.demo.jvmOnly"));
}

#[test]
fn jdk_classes_stay_out_of_wasm_code() {
    // Common code of a Wasm-only package compiles for Wasm, which has no JDK.
    let mut f = fixture_with_jdk(&[("src/Common.kt", "package app\n")], JDK);
    f.packages.platforms = vec![Platforms::WASM, Platforms::NONE];
    assert!(!sees(&f, 0, "java.io.File"));
    f.packages.platforms = vec![Platforms::JVM, Platforms::NONE];
    assert!(sees(&f, 0, "java.io.File"));
}

/// Completes against the real standard library's sources jars, loaded as the server loads them
/// (`KOTLIN_JVM_SOURCES`: `kotlin-stdlib`, `KOTLIN_WASM_SOURCES`: `kotlin-stdlib-wasm-js`). The
/// JVM's `kotlin.concurrent.thread` is offered to JVM code only, and `String.lowercase` to all.
#[test]
#[ignore]
fn platforms_against_the_real_standard_library() {
    use kargo_util::shell::{Shell, Verbosity};
    let home = tempfile::tempdir().unwrap();
    let config = kargo_util::config::Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true);
    let jars = [
        SourcesJar {
            name: "kotlin-stdlib".into(),
            jar: PathBuf::from(std::env::var("KOTLIN_JVM_SOURCES").expect("KOTLIN_JVM_SOURCES")),
            main: Platforms::ALL,
            test: Platforms::NONE,
            java: false,
        },
        SourcesJar {
            name: "kotlin-stdlib-wasm-js".into(),
            jar: PathBuf::from(std::env::var("KOTLIN_WASM_SOURCES").expect("KOTLIN_WASM_SOURCES")),
            main: Platforms::ALL,
            test: Platforms::NONE,
            java: false,
        },
    ];
    let lib = Index::new(true, library::units(&config, &UnitCache::default(), &jars));
    // A package of a JVM and a Wasm target: its common code, the JVM's file and the Wasm one.
    let both = Platforms::JVM.union(Platforms::WASM);
    let cases = [
        (None, both, "fun main() {\n    thr$0\n}\n", "thread", false),
        (None, both, "fun main() {\n    \"A\".lower$0\n}\n", "lowercase", true),
        (Some("jvm"), both, "fun main() {\n    thr$0\n}\n", "thread", true),
        (Some("wasm"), both, "fun main() {\n    thr$0\n}\n", "thread", false),
        (Some("wasm"), both, "fun main() {\n    \"A\".lower$0\n}\n", "lowercase", true),
        // Native takes the common code and the parts of the Wasm standard library it shares with Wasm.
        (Some("native"), Platforms::NATIVE, "fun main() {\n    listOf(1).$0\n}\n", "map", true),
        (Some("native"), Platforms::NATIVE, "fun main() {\n    \"A\".lower$0\n}\n", "lowercase", true),
        (Some("native"), Platforms::NATIVE, "fun main() {\n    thr$0\n}\n", "thread", false),
        (Some("native"), Platforms::NATIVE, "fun main() {\n    Obsolete$0\n}\n", "ObsoleteWorkersApi", true),
        (Some("jvm"), both, "fun main() {\n    Obsolete$0\n}\n", "ObsoleteWorkersApi", false),
    ];
    for (target, platforms, src, label, expected) in cases {
        let at = src.find("$0").unwrap();
        let text = src.replacen("$0", "", 1);
        let origin = Origin::Source(SourceSet { package: 0, kind: SetKind::Main, target: target.map(str::to_owned) });
        let proj = Index::new(false, vec![Arc::new(Unit::new(PathBuf::from("/p/src/Main.kt"), text, origin))]);
        let packages = Packages { visible: vec![vec![0]], platforms: vec![platforms] };
        let an = Analyzer::new(&lib, &proj, &packages);
        let list =
            features::completion(&an, UnitId { lib: false, unit: 0 }, at, CompletionOptions { snippets: true, ..Default::default() }, 0);
        let labels: Vec<&str> = list["items"].as_array().unwrap().iter().filter_map(|i| i["label"].as_str()).collect();
        assert_eq!(labels.contains(&label), expected, "{label} in {target:?} code: {src:?}");
    }
}

#[test]
fn outlines_files() {
    let f = fixture(&[("src/Main.kt", "class A {\n    fun b() {}\n    val c = 1\n}\nfun main() {}\n")]);
    let an = f.an();
    let symbols = features::document_symbols(&an, UnitId { lib: false, unit: 0 });
    assert_eq!(symbols[0]["name"], "A");
    assert_eq!(symbols[0]["kind"], 5);
    assert_eq!(symbols[0]["children"][0]["name"], "b");
    assert_eq!(symbols[0]["children"][0]["kind"], 6);
    assert_eq!(symbols[1]["name"], "main");
    let control = features::Control::default();
    let found = features::workspace_symbols(&an, "mai", false, &control).unwrap();
    assert_eq!(found[0]["name"], "main");
    assert!(found[0]["location"]["range"].is_object());
    // Short queries stay in the project; a longer one reaches the libraries.
    let names = |query: &str| -> Vec<String> {
        let found = features::workspace_symbols(&an, query, false, &control).unwrap();
        found.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_owned()).collect()
    };
    assert!(!names("li").contains(&"listOf".to_owned()));
    assert!(names("listOf").contains(&"listOf".to_owned()));
    // With resolve support the range comes later, from `workspaceSymbol/resolve`.
    let found = features::workspace_symbols(&an, "main", true, &control).unwrap();
    assert!(found[0]["location"]["range"].is_null());
    let resolved = features::resolve_symbol(&an, found[0].clone());
    assert_eq!(resolved["location"]["range"], features::workspace_symbols(&an, "main", false, &control).unwrap()[0]["location"]["range"]);
    let cancelled = || true;
    let control = features::Control { cancelled: Some(&cancelled), progress: None };
    assert!(features::workspace_symbols(&an, "main", false, &control).is_none());
}

/// Runs completion, hover and definition against a real standard library: the extracted
/// sources jar in `KOTLIN_SOURCES`.
#[test]
#[ignore]
fn against_the_real_standard_library() {
    let dir = PathBuf::from(std::env::var("KOTLIN_SOURCES").expect("KOTLIN_SOURCES"));
    let start = std::time::Instant::now();
    let files = kargo_util::fs::collect_files(&dir, &["kt"]);
    let lib_units: Vec<Arc<Unit>> = files
        .iter()
        .map(|p| {
            let rel = p.strip_prefix(&dir).unwrap();
            let folder = rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
            let origin = Origin::Library { name: "kotlin-stdlib".into(), main: folder_platforms(&folder), test: Platforms::NONE };
            Arc::new(Unit::new(p.clone(), std::fs::read_to_string(p).unwrap(), origin))
        })
        .collect();
    let lib = Index::new(true, lib_units);
    eprintln!("indexed {} files in {:?}", files.len(), start.elapsed());
    let cases: &[(&str, &[&str])] = &[
        ("fun main() {\n    \"abc\".$0\n}\n", &["length", "uppercase", "trim", "split", "let", "padStart", "toInt"]),
        ("fun main() {\n    listOf(1, 2).$0\n}\n", &["map", "forEach", "size", "first", "joinToString", "sum", "sortedBy"]),
        ("fun main() {\n    listOf(\"a\").forEach { it.$0 }\n}\n", &["uppercase", "length"]),
        ("fun main() {\n    listOf(\"a\").map { it.length }.first().$0\n}\n", &["plus", "coerceAtLeast"]),
        ("fun main() {\n    buildString { app$0 }\n}\n", &["append", "appendLine"]),
        ("fun main() {\n    mapOf(\"a\" to 1).forEach { (k, v) -> v.$0 }\n}\n", &["plus", "toString"]),
        ("fun main() {\n    for ((k, v) in mapOf(\"a\" to 1)) { k.$0 }\n}\n", &["uppercase"]),
        ("fun main() {\n    for (i in 0 until 10) { i.$0 }\n}\n", &["plus", "coerceIn"]),
        ("fun main() {\n    val m = mutableMapOf<String, Int>()\n    m.$0\n}\n", &["put", "getOrPut", "keys", "entries"]),
        ("fun main() {\n    val sb = StringBuilder()\n    sb.$0\n}\n", &["append", "insert", "length"]),
        ("fun main() {\n    pri$0\n}\n", &["println", "print"]),
        ("fun main() {\n    val r = Regex(\"a\")\n    r.$0\n}\n", &["matches", "find", "replace"]),
        ("fun main() {\n    val x: Int? = null\n    x?.$0\n}\n", &["plus", "let"]),
        ("fun main() {\n    arrayOf(1).$0\n}\n", &["size", "forEach", "map"]),
        ("fun main() {\n    \"a\".lines().$0\n}\n", &["forEach", "size"]),
        ("fun main() {\n    (1..10).$0\n}\n", &["forEach", "first", "step"]),
        ("fun main() {\n    generateSequence(1) { it + 1 }.$0\n}\n", &["take", "map", "toList"]),
        ("import kotlin.math.$0\n", &["sqrt", "PI", "max"]),
        ("fun main() {\n    setOf(1).$0\n}\n", &["contains", "size", "map"]),
        ("fun main() {\n    Result.success(1).$0\n}\n", &["getOrNull", "isSuccess"]),
        ("fun main() {\n    'c'.$0\n}\n", &["isDigit", "code", "uppercaseChar"]),
        ("fun main() {\n    3.14.$0\n}\n", &["toInt", "roundToInt"]),
    ];
    let packages = Packages { visible: vec![vec![0]], platforms: vec![Platforms::JVM] };
    for (src, expected) in cases {
        let at = src.find("$0").unwrap();
        let text = src.replacen("$0", "", 1);
        let origin = Origin::Source(SourceSet { package: 0, kind: SetKind::Main, target: None });
        let proj = Index::new(false, vec![Arc::new(Unit::new(PathBuf::from("/p/src/Main.kt"), text, origin))]);
        let an = Analyzer::new(&lib, &proj, &packages);
        let t = std::time::Instant::now();
        let list =
            features::completion(&an, UnitId { lib: false, unit: 0 }, at, CompletionOptions { snippets: true, ..Default::default() }, 0);
        let labels: Vec<String> = list["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_owned()).collect();
        let missing: Vec<&&str> = expected.iter().filter(|e| !labels.iter().any(|l| l == **e)).collect();
        eprintln!("{:>8.1?} {:>5} items, missing {:?}: {}", t.elapsed(), labels.len(), missing, src.lines().nth(1).unwrap_or(src).trim());
    }
    // Hover and definition.
    for (src, needle) in [
        ("fun main() {\n    print$0ln(\"x\")\n}\n", "fun println"),
        ("fun main() {\n    listOf(1).ma$0p { it }\n}\n", "fun <T, R> Iterable<T>.map"),
        ("fun main() {\n    val s = \"a\".uppercase$0()\n}\n", "fun String.uppercase"),
        ("fun main() {\n    val l = listOf(\"a\")\n    l$0.size\n}\n", "val l: List<String>"),
    ] {
        let at = src.find("$0").unwrap();
        let text = src.replacen("$0", "", 1);
        let origin = Origin::Source(SourceSet { package: 0, kind: SetKind::Main, target: None });
        let proj = Index::new(false, vec![Arc::new(Unit::new(PathBuf::from("/p/src/Main.kt"), text, origin))]);
        let an = Analyzer::new(&lib, &proj, &packages);
        let u = UnitId { lib: false, unit: 0 };
        let tok = an.file(u).lexed.ident_at(at).unwrap();
        let t = std::time::Instant::now();
        let hover = features::hover(&an, u, tok).map(|h| h["contents"]["value"].as_str().unwrap().to_owned()).unwrap_or_default();
        let def = features::targets_at(&an, u, tok).first().and_then(|t| features::location(&an, t));
        eprintln!(
            "{:>8.1?} hover ok={} def={}",
            t.elapsed(),
            hover.contains(needle),
            def.map(|d| d["uri"].to_string()).unwrap_or_default()
        );
        if !hover.contains(needle) {
            eprintln!("{hover}");
        }
    }
}

/// Resolves, hovers and completes at every identifier of the files in `KOTLIN_SOURCES`, as
/// project code against themselves: nothing may panic, and it must stay fast.
#[test]
#[ignore]
fn resolves_every_identifier_of_real_code() {
    let dir = PathBuf::from(std::env::var("KOTLIN_SOURCES").expect("KOTLIN_SOURCES"));
    let files = kargo_util::fs::collect_files(&dir, &["kt"]);
    let lib_units: Vec<Arc<Unit>> = files
        .iter()
        .map(|p| {
            Arc::new(Unit::new(
                p.clone(),
                std::fs::read_to_string(p).unwrap(),
                Origin::Library { name: "lib".into(), main: Platforms::ALL, test: Platforms::NONE },
            ))
        })
        .collect();
    let lib = Index::new(true, lib_units);
    let origin = Origin::Source(SourceSet { package: 0, kind: SetKind::Main, target: None });
    let limit: usize = std::env::var("KOTLIN_FILES").ok().and_then(|n| n.parse().ok()).unwrap_or(40);
    // Generated files of tens of thousands of lines make this loop quadratic.
    let proj_units: Vec<Arc<Unit>> = files
        .iter()
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() < 60_000))
        .take(limit)
        .map(|p| Arc::new(Unit::new(p.clone(), std::fs::read_to_string(p).unwrap(), origin.clone())))
        .collect();
    let proj = Index::new(false, proj_units);
    let packages = Packages { visible: vec![vec![0]], platforms: vec![Platforms::JVM] };
    let start = std::time::Instant::now();
    let (mut idents, mut resolved, mut slowest) = (0, 0, (std::time::Duration::ZERO, String::new()));
    for (ui, unit) in proj.units.iter().enumerate() {
        let u = UnitId { lib: false, unit: ui as u32 };
        let an = Analyzer::new(&lib, &proj, &packages);
        eprintln!("{} ({} tokens)", unit.path.display(), unit.file.lexed.tokens.len());
        for (i, t) in unit.file.lexed.tokens.iter().enumerate() {
            if t.kind != super::lexer::Tok::Ident {
                continue;
            }
            idents += 1;
            let t0 = std::time::Instant::now();
            if !features::targets_at(&an, u, i).is_empty() {
                resolved += 1;
            }
            let _ = features::hover(&an, u, i);
            let _ = features::completion(&an, u, t.end, CompletionOptions::default(), 0);
            let took = t0.elapsed();
            if took > std::time::Duration::from_millis(200) {
                eprintln!("  slow: {took:?} at {} (token {i})", &unit.text[t.span()]);
            }
            if took > slowest.0 {
                slowest = (took, format!("{}:{}", unit.path.display(), &unit.text[t.span()]));
            }
        }
    }
    eprintln!("{idents} identifiers, {resolved} resolved, in {:?}; slowest {:?} at {}", start.elapsed(), slowest.0, slowest.1);
}

/// The JDK's sources for the tests of Java: a few classes of `java.lang`, `java.io`.
const JDK: &[(&str, &str)] = &[
    ("java.base/module-info.java", "module java.base {\n    exports java.io;\n    exports java.lang;\n}\n"),
    (
        "java.base/java/lang/CharSequence.java",
        "package java.lang;\n\npublic interface CharSequence {\n    int length();\n\n    char charAt(int index);\n\n    default java.util.stream.IntStream chars() {\n        return null;\n    }\n}\n",
    ),
    (
        "java.base/java/lang/String.java",
        "package java.lang;\n\npublic final class String implements CharSequence {\n    public int codePointAt(int index) {\n        return 0;\n    }\n\n    public static String valueOf(int i) {\n        return null;\n    }\n}\n",
    ),
    (
        "java.base/java/io/File.java",
        "package java.io;\n\npublic class File {\n    public static final String separator = \"/\";\n\n    public File(String path) {}\n\n    public boolean exists() {\n        return true;\n    }\n\n    public String getName() {\n        return \"\";\n    }\n}\n",
    ),
    ("java.base/java/io/PrintStream.java", "package java.io;\n\npublic class PrintStream {\n    public void println(String line) {}\n}\n"),
    (
        "java.base/java/lang/System.java",
        "package java.lang;\n\npublic final class System {\n    public static final java.io.PrintStream out = null;\n\n    /** Gets a system property. */\n    public static String getProperty(String key) {\n        return null;\n    }\n}\n",
    ),
];

#[test]
fn completes_and_navigates_jdk_classes() {
    // `File` is imported; its members, and those of the classes its members return, complete.
    let f = fixture_with_jdk(&[("src/Main.kt", "import java.io.File\n\nfun main() {\n    File(\"x\").$0\n}\n")], JDK);
    let labels = f.labels();
    assert!(has(&labels, "exists") && has(&labels, "getName"), "{labels:?}");

    // `System` is in `java.lang`, which Kotlin imports; its static members complete through the class.
    let f = fixture_with_jdk(&[("src/Main.kt", "fun main() {\n    System.$0\n}\n")], JDK);
    let labels = f.labels();
    assert!(has(&labels, "out") && has(&labels, "getProperty"), "{labels:?}");

    // Java's `String` is Kotlin's: the members of Kotlin's String complete on what Java returns.
    let f = fixture_with_jdk(&[("src/Main.kt", "fun main() {\n    System.getProperty(\"a\").$0\n}\n")], JDK);
    let labels = f.labels();
    assert!(has(&labels, "uppercase") && has(&labels, "length"), "{labels:?}");

    // Members of a field's type, through the class.
    let f = fixture_with_jdk(&[("src/Main.kt", "fun main() {\n    System.out.$0\n}\n")], JDK);
    assert!(has(&f.labels(), "println"), "{:?}", f.labels());

    // Go to definition opens the JDK's source file, at the method.
    let f = fixture_with_jdk(&[("src/Main.kt", "fun main() {\n    System.getPro$0perty(\"a\")\n}\n")], JDK);
    assert_eq!(f.definition(), Some(("System.java".into(), "public static String getProperty(String key) {".into())));

    // Hover shows the signature, with the Javadoc.
    let f = fixture_with_jdk(&[("src/Main.kt", "fun main() {\n    System.getPro$0perty(\"a\")\n}\n")], JDK);
    let hover = f.hover();
    assert!(hover.contains("getProperty") && hover.contains("Gets a system property"), "{hover}");
}

#[test]
fn knows_the_java_files_of_the_project() {
    let f = fixture(&[
        (
            "src/app/Shape.java",
            "package app;\n\nimport java.util.List;\n\n/** A shape. */\npublic class Shape {\n    public double area() {\n        return 0.0;\n    }\n\n    public List<String> labels() {\n        return null;\n    }\n}\n",
        ),
        ("src/app/Main.kt", "package app\n\nfun main() {\n    Shape().$0\n}\n"),
    ]);
    let labels = f.labels();
    assert!(has(&labels, "area") && has(&labels, "labels"), "{labels:?}");
}

/// Completion and navigation into the JDK on this machine (`JAVA_HOME`), not a fake one. Run with
/// `cargo test -p kargo-lsp -- --ignored completes_the_real_jdk --nocapture`.
#[test]
#[ignore]
fn completes_the_real_jdk() {
    let home = PathBuf::from(std::env::var("JAVA_HOME").expect("JAVA_HOME"));
    let dir = tempfile::tempdir().unwrap();
    let jdk = super::jdk::Jdk::open(&home, &dir.path().join("cache"), "JDK").expect("a JDK with sources");
    let f = fixture_on(
        &[("src/Main.kt", "import java.util.ArrayList\n\nfun main() {\n    ArrayList<String>().$0\n}\n")],
        Some(Arc::new(jdk)),
        Some(dir),
        Vec::new(),
    );
    let labels = f.labels();
    assert!(has(&labels, "add") && has(&labels, "size") && has(&labels, "isEmpty"), "{labels:?}");
    let f = fixture_on(&[("src/Main.kt", "fun main() {\n    System.out.$0\n}\n")], None, None, Vec::new());
    assert!(!has(&f.labels(), "println"), "without a JDK, nothing of System is known");

    // The methods Java adds to Kotlin's String: `chars()` comes from CharSequence's default method.
    let dir = tempfile::tempdir().unwrap();
    let jdk = super::jdk::Jdk::open(&home, &dir.path().join("cache"), "JDK").expect("a JDK with sources");
    let f = fixture_on(&[("src/Main.kt", "fun main() {\n    \"abc\".$0\n}\n")], Some(Arc::new(jdk)), Some(dir), Vec::new());
    let labels = f.labels();
    assert!(has(&labels, "chars") && has(&labels, "codePointAt") && has(&labels, "lines"), "{labels:?}");
}

#[test]
fn completes_the_java_members_of_kotlin_classes() {
    // `String` is Kotlin's class, but the methods Java adds to it complete too, including `chars()`
    // that `CharSequence` declares by default. Static methods don't; a name Kotlin declares is listed once.
    let f = fixture_with_jdk(&[("src/Main.kt", "fun main() {\n    \"abc\".$0\n}\n")], JDK);
    let labels = f.labels();
    assert!(has(&labels, "codePointAt") && has(&labels, "chars"), "{labels:?}");
    assert!(!has(&labels, "valueOf"), "{labels:?}");
    assert_eq!(labels.iter().filter(|l| *l == "length").count(), 1, "{labels:?}");
}

/// The stubs of the hand-made compiled library in [`super::compiled::testing`], as library units.
fn compiled_library() -> Vec<Arc<Unit>> {
    library_units(super::compiled::render_classes(super::compiled::testing::sample_classes(), "generated", None))
}

/// The same library documented from the Javadoc jar of [`super::javadoc::testing`]: standard
/// doclet pages for its Java classes, Dokka's for its Kotlin ones.
fn documented_library() -> Vec<Arc<Unit>> {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("sample-1.0-javadoc.jar");
    super::javadoc::testing::write_jar(&jar);
    let docs = super::javadoc::Docs::read(&jar).unwrap();
    library_units(super::compiled::render_classes(super::compiled::testing::sample_classes(), "generated", Some(&docs)))
}

fn library_units(stubs: Vec<super::compiled::Stub>) -> Vec<Arc<Unit>> {
    stubs
        .into_iter()
        .map(|(rel, text)| {
            Arc::new(Unit::new(
                PathBuf::from("/stubs").join(rel),
                text,
                Origin::Library { name: "sample".into(), main: Platforms::JVM, test: Platforms::NONE },
            ))
        })
        .collect()
}

#[test]
fn completes_libraries_without_sources() {
    let complete = |main: &str| fixture_on(&[("src/Main.kt", main)], None, None, compiled_library()).labels();

    // A Kotlin class read from its metadata, and what its members return.
    let labels = complete("import p.Greeter\n\nfun main() {\n    Greeter(\"a\").$0\n}\n");
    assert!(has(&labels, "greet") && has(&labels, "name") && has(&labels, "pause"), "{labels:?}");
    let labels = complete("import p.Greeter\n\nfun main() {\n    Greeter(\"a\").greet().$0\n}\n");
    assert!(has(&labels, "uppercase"), "{labels:?}");

    // A top-level extension function, but not a hidden one.
    let labels = complete("import p.shout\n\nfun main() {\n    \"a\".$0\n}\n");
    assert!(has(&labels, "shout"), "{labels:?}");
    let labels = complete("package p\n\nfun main() {\n    ol$0\n}\n");
    assert!(!has(&labels, "old"), "{labels:?}");

    // A Java class read from its class file: static members through the class, nested classes.
    let labels = complete("import p.JavaThing\n\nfun main() {\n    JavaThing.$0\n}\n");
    assert!(has(&labels, "of") && has(&labels, "Builder"), "{labels:?}");
    let labels = complete("import p.JavaThing\n\nfun main() {\n    JavaThing.of(1).$0\n}\n");
    assert!(has(&labels, "wrap") && !has(&labels, "secret"), "{labels:?}");

    // Go to definition opens the generated file.
    let f = fixture_on(
        &[("src/Main.kt", "import p.Greeter\n\nfun main() {\n    Greeter(\"a\").gre$0et()\n}\n")],
        None,
        None,
        compiled_library(),
    );
    assert_eq!(f.definition(), Some(("p.kt".into(), "public fun greet(times: Int = ...): String".into())));
}

#[test]
fn documents_libraries_without_sources_from_their_javadoc_jar() {
    let hover = |main: &str| fixture_on(&[("src/Main.kt", main)], None, None, documented_library()).hover();

    // Kotlin declarations, from Dokka's pages: the description, the tags and the sample.
    let h = hover("import p.Greeter\n\nfun main() {\n    Greeter(\"a\").gre$0et()\n}\n");
    for part in ["Greets the person times times.", "how many times, from 1", "the greeting", "println(g.greet(2))"] {
        assert!(h.contains(part), "`{part}` missing from:\n{h}");
    }
    assert!(hover("import p.Gree$0ter\n").contains("Greets people by name."));
    assert!(hover("import p.shout\n\nval s = \"a\".sho$0ut()\n").contains("The string in upper case"));

    // Java classes, from the standard doclet's pages, with the parameter names the class file lacks.
    let h = hover("import p.JavaThing\n\nval t = JavaThing.o$0f(1)\n");
    for part in ["Makes a thing holding `value`.", "the value, at least `0`", "a new thing", "IllegalArgumentException", "`value < 0`"] {
        assert!(h.contains(part), "`{part}` missing from:\n{h}");
    }
    let h = hover("import p.JavaThing\n\nfun f(t: JavaThing) {\n    t.wr$0ap(listOf(1))\n}\n");
    assert!(h.contains("items") && !h.contains("arg0") && h.contains("thing.wrap(List.of(1, 2));"), "{h}");
    assert!(hover("import p.JavaTh$0ing\n").contains("A thing written in Java"));
    assert!(hover("import p.JavaThing\n\nval b: JavaThing.Buil$0der? = null\n").contains("Builds things."));

    // Completion shows it too.
    let f = fixture_on(&[("src/Main.kt", "import p.JavaThing\n\nfun main() {\n    JavaThing.$0\n}\n")], None, None, documented_library());
    let item = f.items().into_iter().find(|i| i["label"] == "of").expect("`of` is offered");
    let item = features::resolve_item(&f.an(), item);
    assert!(item["documentation"]["value"].as_str().unwrap_or_default().contains("Makes a thing holding"), "{item}");
}

#[test]
fn generated_npm_declarations_serve_wasm_code() {
    // What `kargo build` generates for an npm package (see `kargo_ops::dts`), loaded as a Wasm library.
    let express = "package express\n\nimport kotlin.js.*\n\n/**\n * Creates an Express application.\n */\n@JsModule(\"express\")\nexternal fun express(): express.Express\n";
    let types = "package express\n\nimport kotlin.js.*\n\nexternal interface Express : JsAny {\n    fun get(path: String, handlers: (express.Request, express.Response, (JsAny?) -> Unit) -> Unit): express.Express\n}\n\nexternal interface Request : JsAny {\n    val path: String\n}\n\nexternal interface Response : JsAny {\n    fun send(body: String): express.Response\n}\n";
    let libs =
        [("express/Default.kt", express, Platforms::WASM, Platforms::NONE), ("express/Types.kt", types, Platforms::WASM, Platforms::NONE)];
    let main = "import express.express\n\nfun main() {\n    express().get(\"/\") { req, res, _ -> req.pa$0 }\n}\n";
    let f = platform_fixture(&[("src/Main.kt", main)], &libs, vec![Platforms::WASM, Platforms::WASM]);
    assert!(has(&f.labels(), "path"), "{:?}", f.labels());
    let f = platform_fixture(
        &[("src/Main.kt", "import express.express\n\nval app = expr$0ess()\n")],
        &libs,
        vec![Platforms::WASM, Platforms::WASM],
    );
    assert!(f.hover().contains("Creates an Express application."), "{}", f.hover());
    // Not in JVM code.
    let f = platform_fixture(&[("src/Main.kt", "package app\n")], &libs, vec![Platforms::JVM.union(Platforms::WASM), Platforms::JVM]);
    assert!(!sees(&f, 0, "express.express"));
}

// ----- code actions -----

/// The source roots of a package with common sources and `jvm` and `native` ones.
fn multiplatform_roots() -> Vec<(PathBuf, SourceSet)> {
    [None, Some("jvm"), Some("native")]
        .into_iter()
        .map(|target| {
            let dir = target.map_or_else(|| "src".to_owned(), |t| format!("src-{t}"));
            (PathBuf::from("/project").join(dir), SourceSet { package: 0, kind: SetKind::Main, target: target.map(str::to_owned) })
        })
        .collect()
}

impl Fixture {
    /// The code actions at the cursor, with the diagnostics `diagnostics` there.
    fn actions(&self, diagnostics: Vec<Value>, roots: &[(PathBuf, SourceSet)]) -> Vec<Value> {
        let (unit, offset) = self.cursor();
        let manifests = vec![PathBuf::from("/project/kargo.toml")];
        let request = actions::Request {
            unit,
            range: offset..offset,
            only: Vec::new(),
            diagnostics,
            roots,
            manifests: &manifests,
            document_changes: true,
            create_files: true,
        };
        actions::code_actions(&self.an(), &request)
    }

    fn titles(&self, diagnostics: Vec<Value>) -> Vec<String> {
        self.actions(diagnostics, &[]).iter().map(|a| a["title"].as_str().unwrap().to_owned()).collect()
    }

    /// [`Fixture::titles`] without "Generate KDoc", which any declaration without a comment on the
    /// cursor's line offers.
    fn titles_but_kdoc(&self, diagnostics: Vec<Value>) -> Vec<String> {
        self.titles(diagnostics).into_iter().filter(|t| t != "Generate KDoc").collect()
    }

    /// The cursor's file after the edits of the action titled `title`.
    fn applied(&self, diagnostics: Vec<Value>, title: &str) -> String {
        let actions = self.actions(diagnostics, &[]);
        let action = actions.iter().find(|a| a["title"] == title).unwrap_or_else(|| panic!("no `{title}` in {actions:#?}"));
        let unit = &self.proj.units[self.cursor().0.unit as usize];
        let uri = crate::text::path_to_uri(&unit.path);
        apply_edits(&unit.text, action["edit"]["changes"][&uri].as_array().unwrap())
    }

    /// A diagnostic with `message` on the identifier at the cursor.
    fn diagnostic(&self, message: &str) -> Value {
        let (u, tok) = self.token();
        let unit = &self.proj.units[u.unit as usize];
        let span = unit.file.lexed.tokens[tok].span();
        serde_json::json!({ "range": crate::text::range(&unit.text, span), "severity": 1, "source": "kargo", "message": message })
    }
}

fn apply_edits(text: &str, edits: &[Value]) -> String {
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            let start = crate::text::offset(text, &e["range"]["start"]);
            let end = crate::text::offset(text, &e["range"]["end"]);
            (start, end, e["newText"].as_str().unwrap())
        })
        .collect();
    spans.sort_by_key(|(start, end, _)| std::cmp::Reverse((*start, *end)));
    let mut out = text.to_owned();
    for (start, end, new) in spans {
        out.replace_range(start..end, new);
    }
    out
}

#[test]
fn imports_unresolved_names() {
    let lib = ("src/lib/Thing.kt", "package lib\n\nclass Thing\n\nval answer = 42\n");
    let f = fixture(&[lib, ("src/app/Main.kt", "package app\n\nfun main() {\n    val t = Thi$0ng()\n}\n")]);
    assert_eq!(f.titles(Vec::new()), ["Import `lib.Thing`"]);
    assert_eq!(f.applied(Vec::new(), "Import `lib.Thing`"), "package app\n\nimport lib.Thing\n\nfun main() {\n    val t = Thing()\n}\n");

    // In sorted place among the imports.
    let f = fixture(&[lib, ("src/app/Main.kt", "package app\n\nimport a.A\nimport z.Z\n\nfun main() = Thi$0ng()\n")]);
    assert_eq!(
        f.applied(Vec::new(), "Import `lib.Thing`"),
        "package app\n\nimport a.A\nimport lib.Thing\nimport z.Z\n\nfun main() = Thing()\n"
    );

    // A name that isn't a call or a type only from a diagnostic.
    let f = fixture(&[lib, ("src/app/Main.kt", "package app\n\nfun main() = println(ans$0wer)\n")]);
    assert!(f.titles_but_kdoc(Vec::new()).is_empty());
    let diagnostic = f.diagnostic("Unresolved reference 'answer'.");
    let found = f.actions(vec![diagnostic.clone()], &[]);
    assert_eq!(found[0]["title"], "Import `lib.answer`");
    assert_eq!(found[0]["diagnostics"][0], diagnostic);

    // Resolved names get nothing.
    let f = fixture(&[lib, ("src/app/Main.kt", "package app\n\nimport lib.Thing\n\nfun main() = Thi$0ng()\n")]);
    assert!(f.titles_but_kdoc(Vec::new()).is_empty());
}

#[test]
fn organizes_imports() {
    let lib = ("src/lib/Things.kt", "package lib\n\nclass A\nclass B\nclass C\n");
    let main = "package app\n\nimport lib.C\nimport kotlin.math.max\nimport java.io.File\nimport lib.A\nimport lib.A\nimport app.Local\nimport lib.B as Bee\nimport lib.*\n\n/** See [File]. */\nfun main() {$0\n    println(A())\n    println(max(1, 2))\n    println(Bee())\n}\n";
    let f = fixture(&[lib, ("src/app/Main.kt", main)]);
    let organized = f.applied(Vec::new(), "Organize imports");
    assert_eq!(
        organized,
        "package app\n\nimport lib.*\nimport lib.A\nimport java.io.File\nimport kotlin.math.max\nimport lib.B as Bee\n\n/** See [File]. */\nfun main() {\n    println(A())\n    println(max(1, 2))\n    println(Bee())\n}\n"
    );
    // Nothing to do: no action.
    let f = fixture(&[lib, ("src/app/Main.kt", "package app\n\nimport lib.A\n\nfun main() = A()$0\n")]);
    assert!(!f.titles(Vec::new()).iter().any(|t| t == "Organize imports"));
    // Every import unused.
    let f = fixture(&[lib, ("src/app/Main.kt", "package app\n\nimport lib.A\nimport lib.B\n\nfun main() {}$0\n")]);
    assert_eq!(f.applied(Vec::new(), "Organize imports"), "package app\n\nfun main() {}\n");
}

#[test]
fn adds_the_dependency_of_an_unresolved_import() {
    let main = "package app\n\nimport kotlinx.coroutines.launch$0\nimport kotlinx.coroutines.runBlocking\n\nfun main() = runBlocking { launch {} }\n";
    let f = fixture(&[("src/app/Main.kt", main)]);
    let found = f.actions(Vec::new(), &[]);
    let action = found.iter().find(|a| a["command"].is_object()).expect("an add-dependency action");
    assert_eq!(action["title"], "Add `org.jetbrains.kotlinx:kotlinx-coroutines-core` to kargo.toml");
    assert_eq!(action["command"]["command"], crate::actions::ADD_DEPENDENCY);
    let args = &action["command"]["arguments"][0];
    assert_eq!(args["coords"], "org.jetbrains.kotlinx:kotlinx-coroutines-core");
    assert_eq!(args["section"], "dependencies");
    assert_eq!(args["uri"], crate::text::path_to_uri(std::path::Path::new("/project/kargo.toml")));

    // Test code adds to `dev-dependencies`, the longest known prefix wins.
    let f = fixture(&[("tests/AppTest.kt", "import kotlinx.coroutines.test.runTest$0\n")]);
    let found = f.actions(Vec::new(), &[]);
    let args = &found.iter().find(|a| a["command"].is_object()).unwrap()["command"]["arguments"][0];
    assert_eq!(
        (args["coords"].as_str(), args["section"].as_str()),
        (Some("org.jetbrains.kotlinx:kotlinx-coroutines-test"), Some("dev-dependencies"))
    );

    // Not for a package that resolves, or that Kargo doesn't know.
    let f = fixture(&[("src/app/Main.kt", "import kotlin.collections.List$0\nimport com.example.unknown.Thing\n")]);
    assert!(f.actions(Vec::new(), &[]).iter().all(|a| a["command"].is_null()));
    assert_eq!(super::actions::known_module("io.ktor.server.netty.EngineMain"), Some("io.ktor:ktor-server-netty"));
    assert_eq!(super::actions::known_module("io.ktor.server.routing.get"), Some("io.ktor:ktor-server-core"));
    assert_eq!(super::actions::known_module("io.ktorx.Thing"), None);
}

#[test]
fn implements_abstract_members() {
    let repo = "package data\n\nclass User\n\ninterface Repo<T> {\n    fun find(id: Int): T\n    val size: Int\n    var name: String\n    fun describe() = \"repo\"\n    suspend fun save(item: T)\n}\n\nabstract class Base : Repo<User> {\n    override fun find(id: Int): User = User()\n    abstract fun close()\n}\n";
    let f = fixture(&[
        ("src/data/Repo.kt", repo),
        ("src/app/Users.kt", "package app\n\nimport data.Repo\n\nclass Use$0rs : Repo<data.User> {\n}\n"),
    ]);
    let users = f.applied(Vec::new(), "Implement 4 members");
    assert_eq!(
        users,
        "package app\n\nimport data.Repo\nimport data.User\n\nclass Users : Repo<data.User> {\n    override fun find(id: Int): User {\n        TODO(\"Not yet implemented\")\n    }\n\n    override val size: Int\n        get() = TODO(\"Not yet implemented\")\n\n    override var name: String\n        get() = TODO(\"Not yet implemented\")\n        set(value) {}\n\n    override suspend fun save(item: User) {\n        TODO(\"Not yet implemented\")\n    }\n}\n"
    );

    // Through an abstract class that implements some of them; without a body.
    let f = fixture(&[("src/data/Repo.kt", repo), ("src/data/Impl.kt", "package data\n\nclass Im$0pl : Base()\n")]);
    let message = "Class 'Impl' is not abstract and does not implement abstract member 'close'.";
    let diagnostic = f.diagnostic(message);
    let found = f.actions(vec![diagnostic.clone()], &[]);
    let action = found.iter().find(|a| a["title"] == "Implement 4 members").expect("an implement action");
    assert_eq!(action["diagnostics"][0], diagnostic);
    let impl_kt = f.applied(Vec::new(), "Implement 4 members");
    // The closest supertype's members first.
    assert!(impl_kt.starts_with("package data\n\nclass Impl : Base() {\n    override fun close() {\n"), "{impl_kt}");
    assert!(impl_kt.contains("    override val size: Int\n") && !impl_kt.contains("fun find"), "{impl_kt}");

    // Nothing for a class that implements everything, or an abstract one.
    let f = fixture(&[("src/data/Repo.kt", repo), ("src/data/Done.kt", "package data\n\nabstract class Do$0ne : Base()\n")]);
    assert!(f.titles(Vec::new()).is_empty());
}

#[test]
fn adds_missing_actual_declarations() {
    let common = "package app\n\nimport kotlin.time.Duration\n\nexpect fun plat$0form(timeout: Duration = Duration.ZERO): String\n\nexpect class Clock {\n    fun now(): Long\n    val zone: String\n}\n";
    let jvm = "package app\n\nactual fun platform(timeout: Duration): String = \"jvm\"\n";
    let f = fixture(&[("src/app/Platform.kt", common), ("src-jvm/app/Platform.jvm.kt", jvm)]);
    let roots = multiplatform_roots();
    let found = f.actions(Vec::new(), &roots);
    let titles: Vec<&str> = found.iter().map(|a| a["title"].as_str().unwrap()).filter(|t| *t != "Generate KDoc").collect();
    assert_eq!(titles, ["Add `actual` for `platform` to the native target"]);
    let changes = found[0]["edit"]["documentChanges"].as_array().unwrap();
    let uri = crate::text::path_to_uri(std::path::Path::new("/project/src-native/app/Platform.native.kt"));
    assert_eq!(changes[0], serde_json::json!({ "kind": "create", "uri": uri, "options": { "ignoreIfExists": true } }));
    assert_eq!(
        changes[1]["edits"][0]["newText"],
        "package app\n\nimport kotlin.time.Duration\n\nactual fun platform(timeout: Duration): String {\n    TODO(\"Not yet implemented\")\n}\n"
    );

    // A class: appended to the target's file there is, and created in the other.
    let f = fixture(&[
        ("src/app/Platform.kt", &common.replace("$0", "").replace("class Clock", "class Cl$0ock")),
        ("src-jvm/app/Platform.jvm.kt", jvm),
    ]);
    let found = f.actions(Vec::new(), &roots);
    let titles: Vec<&str> = found.iter().map(|a| a["title"].as_str().unwrap()).collect();
    assert_eq!(
        titles,
        [
            "Add `actual` for `Clock` to the jvm target",
            "Add `actual` for `Clock` to the native target",
            "Add `actual` for `Clock` to every target"
        ]
    );
    let edit = &found[0]["edit"]["documentChanges"][0];
    assert_eq!(edit["textDocument"]["uri"], crate::text::path_to_uri(std::path::Path::new("/project/src-jvm/app/Platform.jvm.kt")));
    let appended = apply_edits(jvm, edit["edits"].as_array().unwrap());
    assert_eq!(
        appended,
        format!(
            "{jvm}\nactual class Clock {{\n    actual fun now(): Long {{\n        TODO(\"Not yet implemented\")\n    }}\n\n    actual val zone: String\n        get() = TODO(\"Not yet implemented\")\n}}\n"
        )
    );
    assert_eq!(found[2]["edit"]["documentChanges"].as_array().unwrap().len(), 3);
}

#[test]
fn fixes_compiler_messages() {
    let base =
        "package app\n\nopen class Base {\n    open fun greet() = \"hi\"\n}\n\nclass Child : Base() {\n    fun gr$0eet() = \"hello\"\n}\n";
    let f = fixture(&[("src/app/Child.kt", base)]);
    let diagnostic = f.diagnostic("'fun greet(): String' hides member of supertype 'Base' and needs an 'override' modifier.");
    let fixed = f.applied(vec![diagnostic], "Add `override` to `greet`");
    assert!(fixed.contains("    override fun greet() = \"hello\""), "{fixed}");

    let main = "fun main() {\n    val un$0used = compute()\n    val other = 1\n    println(\"x\")\n}\nfun compute() = 1\n";
    let f = fixture(&[("src/Main.kt", main)]);
    let diagnostic = f.diagnostic("Variable 'unused' is never used.");
    let fixed = f.applied(vec![diagnostic], "Remove variable `unused`");
    assert_eq!(fixed, "fun main() {\n    compute()\n    val other = 1\n    println(\"x\")\n}\nfun compute() = 1\n");
    let f = fixture(&[("src/Main.kt", &main.replace("$0", "").replace("val other", "val ot$0her"))]);
    let diagnostic = f.diagnostic("Unused variable 'other'.");
    let fixed = f.applied(vec![diagnostic], "Remove variable `other`");
    assert_eq!(fixed, "fun main() {\n    val unused = compute()\n    println(\"x\")\n}\nfun compute() = 1\n");

    // A destructured component is renamed to `_`.
    let f = fixture(&[("src/Main.kt", "fun main() {\n    val (a, b$0) = Pair(1, 2)\n    println(a)\n}\n")]);
    let diagnostic = f.diagnostic("Variable 'b' is never used.");
    assert_eq!(f.applied(vec![diagnostic], "Rename to `_`"), "fun main() {\n    val (a, _) = Pair(1, 2)\n    println(a)\n}\n");
}

/// A diagnostic of the server's own with `message` and `tags` on the identifier at the cursor.
fn own_diagnostic(f: &Fixture, message: &str, tags: Value) -> Value {
    let (u, tok) = f.token();
    let unit = &f.proj.units[u.unit as usize];
    let span = unit.file.lexed.tokens[tok].span();
    serde_json::json!({ "range": crate::text::range(&unit.text, span), "severity": 2, "source": "kargo", "tags": tags, "message": message })
}

#[test]
fn fixes_the_server_own_diagnostics() {
    // An unused import is removed with its line.
    let code = "package app\n\nimport kotlin.math.max\nimport kotlin.math.min\n\nfun main() = min(1, 2)$0\n";
    let f = fixture(&[("src/Main.kt", code)]);
    let import = code.find("import kotlin.math.max").unwrap();
    let range = crate::text::range(code, import..import + "import kotlin.math.max".len());
    let diagnostic =
        serde_json::json!({ "range": range, "severity": 2, "source": "kargo", "tags": [1], "message": "unused import `kotlin.math.max`" });
    assert_eq!(
        f.applied(vec![diagnostic], "Remove unused import `kotlin.math.max`"),
        "package app\n\nimport kotlin.math.min\n\nfun main() = min(1, 2)\n"
    );

    // An unused local variable is removed; the compiler's report of it gives the same fix only once.
    let main = "fun main() {\n    val un$0used = 1\n    println(\"x\")\n}\n";
    let f = fixture(&[("src/Main.kt", main)]);
    let diagnostic = own_diagnostic(&f, "variable `unused` is never used", serde_json::json!([1]));
    assert_eq!(f.applied(vec![diagnostic.clone()], "Remove variable `unused`"), "fun main() {\n    println(\"x\")\n}\n");
    let compiler = f.diagnostic("Variable 'unused' is never used.");
    let titles = f.titles(vec![diagnostic, compiler]);
    assert_eq!(titles.iter().filter(|t| *t == "Remove variable `unused`").count(), 1, "{titles:?}");

    // A call to a deprecated declaration becomes its `ReplaceWith`, with the arguments in place of
    // the parameters, parenthesized where they are expressions.
    let code = "@Deprecated(\"use fresh\", ReplaceWith(\"fresh(value + 1)\"))\nfun old(value: Int) = value\n\nfun fresh(v: Int) = v\n\nfun main() {\n    val r = ol$0d(2 * 3)\n}\n";
    let f = fixture(&[("src/Main.kt", code)]);
    let message = "`old` is deprecated: use fresh. Use `fresh(value + 1)` instead";
    let diagnostic = own_diagnostic(&f, message, serde_json::json!([2]));
    assert_eq!(f.applied(vec![diagnostic], "Replace with `fresh(value + 1)`"), code.replace("ol$0d(2 * 3)", "fresh((2 * 3) + 1)"));
    // A named argument is taken by the parameter it names.
    let code = "@Deprecated(\"use fresh\", ReplaceWith(\"fresh(value + 1)\"))\nfun old(value: Int) = value\n\nfun fresh(v: Int) = v\n\nfun main() {\n    val s = o$0ld(value = 4)\n}\n";
    let f = fixture(&[("src/Main.kt", code)]);
    let diagnostic = own_diagnostic(&f, message, serde_json::json!([2]));
    assert_eq!(f.applied(vec![diagnostic], "Replace with `fresh(value + 1)`"), code.replace("o$0ld(value = 4)", "fresh(4 + 1)"));
    // The compiler's own deprecation reports get no fix here: they have no tag.
    let compiler = own_diagnostic(&f, message, serde_json::json!(null));
    assert!(!f.titles(vec![compiler]).iter().any(|t| t.starts_with("Replace with")));
}

// ----- annotations -----

/// Spring's annotations, as the Java sources a library's sources jar provides.
fn spring_libs() -> Vec<Arc<Unit>> {
    let files = [
        (
            "org/springframework/web/bind/annotation/GetMapping.java",
            "package org.springframework.web.bind.annotation;\n\n@Target(ElementType.METHOD)\n@Retention(RetentionPolicy.RUNTIME)\npublic @interface GetMapping {\n    String[] value() default {};\n}\n",
        ),
        (
            "org/springframework/web/bind/annotation/RequestParam.java",
            "package org.springframework.web.bind.annotation;\n\npublic @interface RequestParam {\n    String name() default \"\";\n    String value() default \"\";\n    boolean required() default true;\n    String defaultValue() default \"\";\n}\n",
        ),
        (
            "org/springframework/web/bind/annotation/RestController.java",
            "package org.springframework.web.bind.annotation;\n\npublic @interface RestController {\n}\n",
        ),
    ];
    files
        .into_iter()
        .map(|(path, text)| {
            Arc::new(Unit::new(
                PathBuf::from("/spring").join(path),
                text.to_owned(),
                Origin::Library { name: "spring-web".into(), main: Platforms::JVM, test: Platforms::NONE },
            ))
        })
        .collect()
}

const SPRING_IMPORTS: &str =
    "import org.springframework.web.bind.annotation.GetMapping\nimport org.springframework.web.bind.annotation.RequestParam\n";

#[test]
fn completes_annotation_classes_after_at() {
    let main = "package app\n\nfun restart() = 1\n\n@Rest$0\nclass G\n";
    let f = fixture_on(&[("src/Main.kt", main)], None, None, spring_libs());
    let items = f.items();
    let labels: Vec<String> = items.iter().map(|i| i["label"].as_str().unwrap().to_owned()).collect();
    assert!(has(&labels, "RestController"), "{labels:?}");
    // Only annotation classes: the function `restart` matches too, but is not offered.
    assert!(!has(&labels, "restart"), "{labels:?}");
    let controller = items.iter().find(|i| i["label"] == "RestController").unwrap();
    assert_eq!(
        controller["additionalTextEdits"][0]["newText"], "\n\nimport org.springframework.web.bind.annotation.RestController",
        "{controller}"
    );

    // A qualified name takes annotations only, too.
    let f = fixture_on(
        &[("src/Main.kt", "package app\n\n@org.springframework.web.bind.annotation.Rest$0\nclass G\n")],
        None,
        None,
        spring_libs(),
    );
    assert!(has(&f.labels(), "RestController"), "{:?}", f.labels());
}

#[test]
fn completes_and_navigates_annotation_arguments() {
    // Completion offers the arguments of the annotation whose `(` comes before the cursor.
    let f = fixture_on(
        &[("src/Main.kt", &format!("package app\n\n{SPRING_IMPORTS}\n@RequestParam(def$0)\nfun greet(name: String) = name\n"))],
        None,
        None,
        spring_libs(),
    );
    let items = f.items();
    let default = items.iter().find(|i| i["label"] == "defaultValue").expect("the defaultValue argument");
    assert_eq!(default["textEdit"]["newText"], "defaultValue = ", "{default}");
    assert!(!has(&f.labels(), "greet"), "{:?}", f.labels());

    // Arguments already given are not offered again.
    let f = fixture_on(
        &[("src/Main.kt", &format!("package app\n\n{SPRING_IMPORTS}\n@RequestParam(name = \"a\", $0)\nfun greet(name: String) = name\n"))],
        None,
        None,
        spring_libs(),
    );
    let labels = f.labels();
    assert!(has(&labels, "value") && has(&labels, "defaultValue"), "{labels:?}");
    assert!(!has(&labels, "name"), "{labels:?}");

    // Hover and go to definition on an argument of a Java annotation: its element.
    let f = fixture_on(
        &[(
            "src/Main.kt",
            &format!(
                "package app\n\n{SPRING_IMPORTS}\n@GetMapping(\"/\")\nfun greet(@RequestParam(defaultV$0alue = \"World\") name: String) = name\n"
            ),
        )],
        None,
        None,
        spring_libs(),
    );
    assert!(f.hover().contains("defaultValue"), "{}", f.hover());
    assert_eq!(f.definition(), Some(("RequestParam.java".into(), "String defaultValue() default \"\";".into())));

    // Arguments of a Kotlin annotation class are its constructor parameters.
    let f = fixture_on(
        &[
            ("src/Tag.kt", "package app\n\nannotation class Tag(val label: String)\n"),
            ("src/Main.kt", "package app\n\n@Tag(la$0bel = \"x\")\nfun main() {}\n"),
        ],
        None,
        None,
        Vec::new(),
    );
    assert_eq!(f.definition(), Some(("Tag.kt".into(), "annotation class Tag(val label: String)".into())));
}

#[test]
fn hovers_and_navigates_annotations_wherever_they_are() {
    // On a class, a function, a parameter and a property.
    let cases = [
        format!("package app\n\n{SPRING_IMPORTS}\n@Req$0uestParam(\"a\") class G\n"),
        format!("package app\n\n{SPRING_IMPORTS}\n@GetMapping(\"/\")\nfun greet(@Req$0uestParam(\"a\") name: String) = name\n"),
        format!("package app\n\n{SPRING_IMPORTS}\nclass G {{\n    @Req$0uestParam(\"a\") val name = 1\n}}\n"),
        format!("package app\n\n{SPRING_IMPORTS}\nfun main() {{\n    @Req$0uestParam(\"a\") val name = 1\n}}\n"),
    ];
    for case in cases {
        let f = fixture_on(&[("src/Main.kt", case.as_str())], None, None, spring_libs());
        assert_eq!(f.definition(), Some(("RequestParam.java".into(), "public @interface RequestParam {".into())), "{case}");
        assert!(f.hover().contains("RequestParam"), "{case}: {}", f.hover());
    }
}

/// The annotations of the real Spring Web sources (`SPRING_WEB_SOURCES`: the `spring-web` sources
/// jar), loaded as a dependency's sources jar is: they complete after `@`, show their Javadoc, and
/// their arguments complete and navigate.
#[test]
#[ignore]
fn annotations_against_the_real_spring_web_sources() {
    use kargo_util::shell::{Shell, Verbosity};
    let home = tempfile::tempdir().unwrap();
    let config = kargo_util::config::Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true);
    let jars = [SourcesJar {
        name: "spring-web".into(),
        jar: PathBuf::from(std::env::var("SPRING_WEB_SOURCES").expect("SPRING_WEB_SOURCES")),
        main: Platforms::JVM,
        test: Platforms::NONE,
        java: true,
    }];
    let libs = library::units(&config, &UnitCache::default(), &jars);
    let imports =
        "import org.springframework.web.bind.annotation.GetMapping\nimport org.springframework.web.bind.annotation.RequestParam\n";

    let f = fixture_with(&[("src/Main.kt", "package app\n\n@Rest$0\nclass G\n")], libs.clone(), None, None);
    assert!(has(&f.labels(), "RestController"), "{:?}", f.labels());

    let main =
        format!("package app\n\n{imports}\n@GetMapping(\"/\")\nfun greet(@RequestParam(defaultV$0alue = \"World\") name: String) = name\n");
    let f = fixture_with(&[("src/Main.kt", &main)], libs.clone(), None, None);
    assert!(f.hover().contains("The default value to use as a fallback"), "{}", f.hover());
    assert_eq!(f.definition().map(|(file, _)| file), Some("RequestParam.java".to_owned()));

    let main = format!("package app\n\n{imports}\n@RequestParam(def$0)\nfun greet(name: String) = name\n");
    let f = fixture_with(&[("src/Main.kt", &main)], libs, None, None);
    assert!(has(&f.labels(), "defaultValue"), "{:?}", f.labels());
}

#[test]
fn smart_casts_to_unrelated_types_keep_both_types() {
    let decl = "interface Named {\n    fun name(): String\n}\nopen class Shape\nclass Circle : Shape(), Named {\n    override fun name() = \"c\"\n    val radius = 1\n}\n";
    let labels = |body: &str| fixture(&[("src/Main.kt", &format!("{decl}{body}"))]).labels();
    // A shape that is also named: the members of both, and not of the class it could be.
    let l = labels("fun f(shape: Shape) {\n    if (shape is Named) shape.$0\n}\n");
    assert!(has(&l, "name") && has(&l, "toString") && !has(&l, "radius"), "{l:?}");
    // A type parameter checked against a class that is not its bound.
    let l = labels("fun <T : Comparable<T>> f(x: T) {\n    if (x is CharSequence) x.$0\n}\n");
    assert!(has(&l, "length") && has(&l, "compareTo"), "{l:?}");
    // A subtype narrows to itself, as before.
    let l = labels("fun f(shape: Shape) {\n    if (shape is Circle) shape.$0\n}\n");
    assert!(has(&l, "radius") && has(&l, "name"), "{l:?}");
}

#[test]
fn follows_smart_casts() {
    let decls = "open class Shape\nclass Circle(val radius: Int) : Shape()\nclass Square(val side: Int) : Shape()\n";
    let at = |body: &str| fixture(&[("src/Main.kt", &format!("{decls}{body}"))]).labels();
    let circle = |body: &str| has(&at(body), "radius");

    assert!(circle("fun f(shape: Shape) {\n    if (shape is Circle) shape.$0\n}\n"));
    assert!(circle("fun f(shape: Shape) {\n    if (shape is Circle) {\n        shape.$0\n    }\n}\n"));
    assert!(circle("fun f(shape: Shape) {\n    if (shape is Circle && shape.$0) {}\n}\n"));
    assert!(circle("fun f(shape: Shape) {\n    when (shape) {\n        is Circle -> shape.$0\n        else -> {}\n    }\n}\n"));
    assert!(circle("fun f(shape: Shape) {\n    when {\n        shape is Circle -> shape.$0\n    }\n}\n"));
    assert!(circle("fun f(shape: Shape) {\n    if (shape !is Circle) return\n    shape.$0\n}\n"));
    assert!(circle("fun f(shape: Shape) {\n    if (shape !is Circle) { println() } else shape.$0\n}\n"));
    assert!(circle("fun f(shape: Shape) {\n    shape as Circle\n    shape.$0\n}\n"));
    assert!(circle("fun f(shape: Shape) {\n    require(shape is Circle)\n    shape.$0\n}\n"));
    assert!(circle("fun f(shape: Shape) {\n    val s = shape\n    if (s is Circle) s.$0\n}\n"));

    // Outside the check, and for what the compiler does not narrow.
    assert!(!circle("fun f(shape: Shape) {\n    if (shape is Circle) {}\n    shape.$0\n}\n"));
    assert!(!circle("fun f(shape: Shape) {\n    if (shape !is Circle) println()\n    shape.$0\n}\n"));
    assert!(!circle("fun f(shape: Shape) {\n    when (shape) {\n        is Square -> {}\n        else -> shape.$0\n    }\n}\n"));
    assert!(!circle("fun f(shape: Shape) {\n    var s = shape\n    if (s is Circle) s.$0\n}\n"));
    assert!(!circle("fun f(shape: Shape, other: Shape) {\n    if (other is Circle) shape.$0\n}\n"));
    assert!(at("fun f(shape: Shape) {\n    when (shape) {\n        is Square -> shape.$0\n    }\n}\n").contains(&"side".to_owned()));
}

#[test]
fn tracks_nullable_receivers() {
    let at = |body: &str| fixture(&[("src/Main.kt", body)]).labels();
    let length = |body: &str| has(&at(body), "length");

    // `.` on a nullable value offers the extensions of nullable types only; `?.` unwraps it.
    assert!(!length("fun f(s: String?) {\n    s.$0\n}\n"));
    assert!(has(&at("fun f(s: String?) {\n    s.$0\n}\n"), "let"));
    let l = at("fun f(s: String?) {\n    s.$0\n}\n");
    assert!(has(&l, "isNullOrEmpty"), "{l:?}");
    assert!(length("fun f(s: String?) {\n    s?.$0\n}\n"));
    assert!(length("fun f(s: String) {\n    s.$0\n}\n"));
    // A safe call's result is nullable again.
    assert!(!length("fun f(s: String?) {\n    s?.trim().$0\n}\n"));
    assert!(length("fun f(s: String?) {\n    s?.trim()?.$0\n}\n"));
    assert!(length("fun f(s: String?) {\n    s!!.$0\n}\n"));

    // Null checks narrow parameters and `val`s.
    assert!(length("fun f(s: String?) {\n    if (s != null) s.$0\n}\n"));
    assert!(length("fun f(s: String?) {\n    if (s != null && s.$0) {}\n}\n"));
    assert!(length("fun f(s: String?) {\n    if (s == null) return\n    s.$0\n}\n"));
    assert!(length("fun f(s: String?) {\n    if (s == null) println() else s.$0\n}\n"));
    assert!(length("fun f(s: String?) {\n    s!!\n    s.$0\n}\n"));
    assert!(length("fun f(s: String?) {\n    s ?: return\n    s.$0\n}\n"));
    assert!(!length("fun f(s: String?) {\n    if (s != null) {}\n    s.$0\n}\n"));
    assert!(!length("fun f(s: String?) {\n    var t = s\n    if (t != null) {}\n    t.$0\n}\n"));
}

/// Whether completion at the cursor of `body` offers `name`.
fn offers(body: &str, name: &str) -> bool {
    has(&fixture(&[("src/Main.kt", body)]).labels(), name)
}

#[test]
fn infers_when_try_and_if_branches() {
    let decls = "open class Shape\nclass Circle(val radius: Int) : Shape()\nclass Square(val side: Int) : Shape()\n";
    let body = |code: &str| format!("{decls}fun f(n: Int) {{\n{code}\n}}\n");

    assert!(offers(&body("val s = when (n) {\n 1 -> \"a\"\n else -> \"b\"\n}\ns.$0"), "length"));
    assert!(offers(&body("val s = when {\n n > 1 -> { println(); \"a\" }\n else -> throw IllegalStateException()\n}\ns.$0"), "length"));
    assert!(offers(&body("when (n) {\n 1 -> \"a\"\n else -> \"b\"\n}.$0"), "length"));
    // Branches of different classes: the supertype they share.
    assert!(offers(&body("val s = when (n) {\n 1 -> Circle(1)\n else -> Square(2)\n}\ns.$0"), "toString"));
    assert!(!offers(&body("val s = when (n) {\n 1 -> Circle(1)\n else -> Square(2)\n}\ns.$0"), "radius"));
    assert!(offers(&body("val s = when (n) {\n 1 -> Circle(1)\n else -> Circle(2)\n}\ns.$0"), "radius"));
    assert!(offers(&body("val s = if (n > 1) Circle(1) else Square(2)\ns.$0"), "toString"));
    assert!(!offers(&body("val s = if (n > 1) Circle(1) else Square(2)\ns.$0"), "radius"));
    assert!(offers(&body("val s = if (n > 1) throw Exception() else \"a\"\ns.$0"), "length"));
    assert!(offers(&body("val s = if (n > 1) { 1 } else if (n > 0) { 2 } else { 3 }\ns.$0"), "shl"));

    assert!(offers(&body("val s = try {\n \"a\"\n} catch (e: Exception) {\n \"b\"\n} finally {\n println()\n}\ns.$0"), "length"));
    assert!(offers(&body("val s = try { Circle(1) } catch (e: Exception) { Square(2) }\ns.$0"), "toString"));
    assert!(offers(&body("val s = try { throw Exception() } catch (e: Exception) { \"b\" }\ns.$0"), "length"));
    assert!(offers(&body("val s = try { \"a\" } catch (e: Exception) { return }\ns.$0"), "length"));
}

#[test]
fn infers_the_operators_of_custom_types() {
    let decls = "class Vec(val x: Int) {\n    operator fun plus(o: Vec): Vec = this\n    operator fun times(k: Int): Scaled = Scaled()\n    operator fun times(o: Vec): String = \"\"\n    operator fun unaryMinus(): Vec = this\n    operator fun rangeTo(o: Vec): Span = Span()\n    operator fun get(i: Int): String = \"\"\n}\nclass Scaled { val factor = 1 }\nclass Span { val length2 = 1 }\n";
    let body = |code: &str| format!("{decls}fun f(a: Vec, b: Vec) {{\n{code}\n}}\n");

    assert!(offers(&body("(a + b).$0"), "x"));
    assert!(offers(&body("val t = a * 2\nt.$0"), "factor"));
    assert!(offers(&body("val t = a * b\nt.$0"), "length"));
    assert!(offers(&body("val t = a + b * 2\nt.$0"), "x"));
    assert!(offers(&body("val t = -a\nt.$0"), "x"));
    assert!(offers(&body("val t = a..b\nt.$0"), "length2"));
    assert!(offers(&body("val t = a[0]\nt.$0"), "length"));
    // Standard types keep their own.
    assert!(offers(&body("val t = 1 + 2\nt.$0"), "shl"));
    assert!(offers(&body("val t = \"a\" + 1\nt.$0"), "length"));
    assert!(offers(&body("val t = 1L + 2L\nt.$0"), "toInt"));
}

#[test]
fn infers_function_references_and_lambdas_as_values() {
    let at = |code: &str| {
        format!(
            "fun twice(n: Int): String = \"\"\nclass A {{\n    fun name(): String = \"\"\n    val size: Int = 0\n}}\nfun f(a: A) {{\n{code}\n}}\n"
        )
    };

    assert!(offers(&at("val g = ::twice\ng(1).$0"), "length"));
    assert!(offers(&at("val g = String::length\ng(\"a\").$0"), "shl"));
    assert!(offers(&at("val g = A::name\ng(a).$0"), "length"));
    assert!(offers(&at("val g = a::name\ng().$0"), "length"));
    assert!(offers(&at("val g = A::size\ng(a).$0"), "shl"));
    assert!(offers(&at("val g = { x: Int -> \"v\" + x }\ng(1).$0"), "length"));
    assert!(offers(&at("val g = { x: Int -> x }\ng(1).$0"), "shl"));
    assert!(offers(&at("val g = { \"a\" }\ng().$0"), "length"));
    assert!(offers(&at("listOf(1).map(::twice).first().$0"), "length"));
    assert!(offers(&at("listOf(a).map(A::name).first().$0"), "length"));
}

#[test]
fn expands_function_type_aliases_for_lambdas() {
    let code = "class Event(val id: Int)\ntypealias Handler = (Event) -> Unit\ntypealias Mapper<T> = (T) -> String\nfun on(h: Handler) {}\nfun <T> convert(m: Mapper<T>, v: T) {}\nfun f() {\n";
    assert!(offers(&format!("{code}    on {{ it.$0 }}\n}}\n"), "id"));
    assert!(offers(&format!("{code}    on {{ e -> e.$0 }}\n}}\n"), "id"));
    assert!(offers(&format!("{code}    convert({{ it.$0 }}, \"a\")\n}}\n"), "length"));
}

#[test]
fn uses_upper_bounds_of_type_parameters() {
    assert!(offers("fun <T : CharSequence> f(x: T) {\n    x.$0\n}\n", "length"));
    assert!(offers("fun <T : CharSequence> f(x: T?) {\n    x?.$0\n}\n", "length"));
    assert!(offers("class Box<T : CharSequence>(val v: T) {\n    fun f() = v.$0\n}\n", "length"));
    assert!(!offers("fun <T> f(x: T) {\n    x.$0\n}\n", "length"));
    assert!(offers("fun <T : Comparable<T>> f(x: T) {\n    x.$0\n}\n", "compareTo"));
}

#[test]
fn offers_the_members_of_every_generic_upper_bound() {
    let labels = completes("fun <T> f(x: T) where T : CharSequence, T : Comparable<T> {\n    x.$0\n}\n");
    assert!(has(&labels, "length") && has(&labels, "compareTo"), "{labels:?}");
    let labels = completes("fun <T : CharSequence> f(x: T) where T : Comparable<T> {\n    x.$0\n}\n");
    assert!(has(&labels, "length") && has(&labels, "compareTo"), "a bound and a where clause: {labels:?}");
    let labels = completes("fun <T> f(x: T) where T : CharSequence, T : Comparable<T> {\n    x.compareTo(x).$0\n}\n");
    assert!(has(&labels, "toString"), "{labels:?}");
}

#[test]
fn projects_star_and_declaration_site_variance() {
    // A star projection reads as `Any?`: its members are the ones of `Any`.
    let labels = completes("fun f(l: List<*>) {\n    l.first()?.$0\n}\n");
    assert!(has(&labels, "hashCode") && !has(&labels, "length"), "{labels:?}");
    assert!(offers("fun f(l: List<out CharSequence>) {\n    l.first().$0\n}\n", "length"));
    assert!(offers("class Source<out T>(val value: T)\nfun f(s: Source<String>) {\n    s.value.$0\n}\n", "length"));
    assert!(offers("class Source<out T>(val value: T)\nfun f(s: Source<*>) {\n    s.value?.$0\n}\n", "hashCode"));
    assert!(!offers("class Source<out T>(val value: T)\nfun f(s: Source<*>) {\n    s.value?.$0\n}\n", "length"));
}

#[test]
fn binds_type_parameters_from_generic_receivers() {
    let decl = "fun <T : Comparable<T>> List<T>.best(): T = first()\nfun <T> T.keep(): T = this\n";
    assert!(offers(&format!("{decl}fun f() {{\n    listOf(\"a\").best().$0\n}}\n"), "length"));
    assert!(offers(&format!("{decl}fun f() {{\n    \"a\".keep().$0\n}}\n"), "length"));
    assert!(offers("fun f() {\n    \"a\".also { it.$0 }\n}\n", "length"));
}

#[test]
fn self_referential_bounds_resolve_without_recursion() {
    let decl = "abstract class Builder<B : Builder<B>> {\n    fun name(): String = \"\"\n}\n";
    assert!(offers(&format!("{decl}fun <B : Builder<B>> f(b: B) {{\n    b.$0\n}}\n"), "name"));
    assert!(offers("fun <E : Enum<E>> f(e: E) {\n    e.$0\n}\n", "ordinal"));
    assert!(offers("fun <T : Comparable<T>> f(x: T) {\n    x.$0\n}\n", "compareTo"));
}

#[test]
fn expands_generic_type_aliases() {
    let decl = "typealias Pairs<T> = List<Pair<T, T>>\n";
    assert!(offers(&format!("{decl}fun f(p: Pairs<String>) {{\n    p.first().first.$0\n}}\n"), "length"));
    assert!(offers(&format!("{decl}fun f(p: Pairs<String>) {{\n    p.first().second.$0\n}}\n"), "length"));
}

#[test]
fn offers_the_members_and_underlying_type_of_value_classes() {
    let decl = "@JvmInline\nvalue class UserId(val raw: String) {\n    fun shout() = raw.uppercase()\n}\n";
    assert!(offers(&format!("{decl}fun f(id: UserId) {{\n    id.$0\n}}\n"), "shout"));
    assert!(offers(&format!("{decl}fun f(id: UserId) {{\n    id.raw.$0\n}}\n"), "length"));
}

#[test]
fn infers_reified_type_parameters_from_explicit_arguments() {
    let decl = "inline fun <reified T> make(): T = TODO()\n";
    assert!(offers(&format!("{decl}fun f() {{\n    make<String>().$0\n}}\n"), "length"));
}

#[test]
fn infers_type_arguments_of_nested_calls() {
    assert!(offers("fun f() {\n    mapOf(\"a\" to \"x\").values.first().$0\n}\n", "length"));
    assert!(offers("fun f() {\n    mapOf(\"a\" to \"x\").entries.first().$0\n}\n", "key"));
    assert!(offers("fun f() {\n    listOf(\"a\", \"b\").map { it.length }.first().$0\n}\n", "plus"));
    assert!(offers("fun f() {\n    listOf(1, 2).first().$0\n}\n", "toInt"));
    assert!(offers("fun f() {\n    listOf(1, \"a\").first().$0\n}\n", "toString"));
    // Builder inference: the element is what the body adds to the receiver.
    assert!(offers("fun f() {\n    buildList {\n        add(\"x\")\n    }.first().$0\n}\n", "length"));
    assert!(offers("fun f() {\n    buildList {\n        add(1)\n    }.first().$0\n}\n", "toInt"));
    assert!(offers("fun f() {\n    buildList<String> {\n        add(\"x\")\n    }.first().$0\n}\n", "length"));
}

#[test]
fn keeps_nullable_type_arguments() {
    let decl = "fun <T> keep(x: T?): T? = x\n";
    assert!(offers(&format!("{decl}fun f(s: String?) {{\n    keep(s)?.$0\n}}\n"), "length"));
    assert!(offers("fun f(m: Map<String, String>) {\n    m[\"a\"]?.$0\n}\n", "length"));
    assert!(offers("fun f(m: Map<String, String>) {\n    m.get(\"a\")?.$0\n}\n", "length"));
    assert!(offers("fun f(m: Map<String, String?>) {\n    m[\"a\"]?.$0\n}\n", "length"));
}

#[test]
fn takes_type_arguments_from_the_declared_type() {
    assert!(offers("fun f() {\n    val list: List<String> = emptyList()\n    list.first().$0\n}\n", "length"));
    assert!(offers("fun f() {\n    val list: MutableList<String> = mutableListOf()\n    list.first().$0\n}\n", "length"));
    assert!(offers("fun names(): List<String> = emptyList()\nfun f() {\n    names().first().$0\n}\n", "length"));
}

#[test]
fn knows_the_members_of_object_expressions() {
    let decls = "interface Foo {\n    fun base(): Int\n}\n";
    let object = "val o = object : Foo {\n        override fun base() = 1\n        val extra = \"x\"\n    }\n";
    assert!(offers(&format!("{decls}fun f() {{\n    {object}    o.$0\n}}\n"), "extra"));
    assert!(offers(&format!("{decls}fun f() {{\n    {object}    o.$0\n}}\n"), "base"));
    assert!(offers(&format!("{decls}fun f() {{\n    {object}    o.extra.$0\n}}\n"), "length"));
    assert!(offers(
        &format!("{decls}fun f() {{\n    object : Foo {{\n        override fun base() = 1\n        fun own() = this.$0\n    }}\n}}\n"),
        "base"
    ));
    // A class declared inside a function.
    assert!(offers("fun f() {\n    class Local {\n        val v = \"x\"\n    }\n    Local().$0\n}\n", "v"));
    assert!(offers("fun f() {\n    class Local {\n        val v = \"x\"\n    }\n    val l = Local()\n    l.v.$0\n}\n", "length"));
}

#[test]
fn infers_delegated_properties() {
    let at = |code: &str| {
        format!(
            "import kotlin.properties.Delegates\nclass Holder(val map: Map<String, Any?>) {{\n{code}\n    fun f() {{\n        value.$0\n    }}\n}}\n"
        )
    };
    assert!(offers(&at("    val value by lazy { \"x\" }"), "length"));
    assert!(offers(&at("    var value by Delegates.observable(\"x\") { _, _, _ -> }"), "length"));
    assert!(offers(&at("    var value by Delegates.vetoable(\"x\") { _, _, _ -> true }"), "length"));
    assert!(offers(&at("    var value by Delegates.notNull<String>()"), "length"));
    assert!(offers(
        &at(
            "    val value by Own()\n    class Own {\n        operator fun getValue(r: Any?, p: kotlin.reflect.KProperty<*>): String = \"\"\n    }"
        ),
        "length"
    ));
    // The type of a map-delegated property follows the map's values.
    assert!(offers("class H(val map: Map<String, String>) {\n    val value by map\n    fun f() {\n        value.$0\n    }\n}\n", "length"));
}

#[test]
fn resolves_labels_of_lambdas() {
    let code = "fun run2(block: StringBuilder.() -> Unit) {}\nfun f() {\n    run2 outer@{\n        val g = { this@outer.$0 }\n    }\n}\n";
    assert!(offers(code, "append"));
    let code = "fun f() {\n    with(StringBuilder()) label@{\n        this@label.$0\n    }\n}\n";
    assert!(offers(code, "append"));
    let code = "fun f() {\n    with(StringBuilder()) {\n        this@with.$0\n    }\n}\n";
    assert!(offers(code, "append"));
}

/// The classified identifiers of the first file, as `(text, type, modifiers)`.
fn classified(f: &Fixture) -> Vec<(String, &'static str, Vec<&'static str>)> {
    use super::semantic::{self, MODIFIERS, TYPES};
    let u = UnitId { lib: false, unit: 0 };
    let text = &f.proj.units[0].text;
    semantic::classify(&f.an(), u)
        .into_iter()
        .map(|c| {
            let mods = (0..MODIFIERS.len()).filter(|b| c.modifiers & (1 << b) != 0).map(|b| MODIFIERS[b]).collect();
            (text[c.span].to_owned(), TYPES[c.kind as usize], mods)
        })
        .collect()
}

#[test]
fn classifies_identifiers_by_what_they_resolve_to() {
    let f = fixture(&[(
        "src/Main.kt",
        "package app\n\nclass Box<T>(val item: T, var count: Int) {\n    fun open(arg: T): T = arg\n    companion object { val ZERO = 0 }\n}\n\n@Deprecated(\"no\")\nfun old() {}\n\nfun main() {\n    val b = Box(1, 2)\n    var n = b.count\n    b.open(n)\n    println(Box.ZERO)\n    old()\n}\n",
    )]);
    let tokens = classified(&f);
    let find = |name: &str, nth: usize| tokens.iter().filter(|t| t.0 == name).nth(nth).cloned().unwrap_or_else(|| panic!("{name} {nth}"));
    assert_eq!(find("Box", 0), ("Box".into(), "class", vec!["declaration"]));
    assert_eq!(find("T", 0), ("T".into(), "typeParameter", vec!["declaration"]));
    assert_eq!(find("item", 0), ("item".into(), "property", vec!["declaration", "readonly"]));
    assert_eq!(find("count", 0).2, vec!["declaration"]);
    assert_eq!(find("open", 0), ("open".into(), "method", vec!["declaration"]));
    assert_eq!(find("arg", 0), ("arg".into(), "parameter", vec!["declaration", "readonly"]));
    assert_eq!(find("T", 1).1, "typeParameter");
    assert_eq!(find("ZERO", 0), ("ZERO".into(), "property", vec!["declaration", "readonly", "static"]));
    assert_eq!(find("old", 1), ("old".into(), "function", vec!["deprecated"]));
    assert_eq!(find("b", 1), ("b".into(), "variable", vec!["readonly"]));
    assert_eq!(find("n", 1), ("n".into(), "variable", vec![]));
    assert_eq!(find("count", 1), ("count".into(), "property", vec![]));
    assert_eq!(find("open", 1).1, "method");
    assert_eq!(find("println", 0), ("println".into(), "function", vec!["defaultLibrary"]));
}

#[test]
fn encodes_semantic_tokens_relative_to_the_previous_one() {
    use super::semantic;
    let f = fixture(&[("src/Main.kt", "fun a() {}\nfun b() { a() }\n")]);
    let u = UnitId { lib: false, unit: 0 };
    let text = &f.proj.units[0].text;
    let tokens = semantic::classify(&f.an(), u);
    // function (0,4) a; (1,4) b; (1,10) a
    let all = semantic::encode(text, &tokens, None);
    assert_eq!(all["data"], serde_json::json!([0, 4, 1, 10, 1, 1, 4, 1, 10, 1, 0, 6, 1, 10, 0]));
    let second_line = semantic::encode(text, &tokens, Some(11..text.len()));
    assert_eq!(second_line["data"], serde_json::json!([1, 4, 1, 10, 1, 0, 6, 1, 10, 0]));
}

fn folds(src: &str) -> Vec<(u64, u64, Option<String>)> {
    let file = super::syntax::parse(src);
    super::ranges::folding_ranges(&file, src)
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["startLine"].as_u64().unwrap(), r["endLine"].as_u64().unwrap(), r["kind"].as_str().map(str::to_owned)))
        .collect()
}

#[test]
fn folds_bodies_imports_comments_and_strings() {
    let src = "import a.B\nimport a.C\n\n/**\n * Doc.\n */\nclass K {\n    fun f() {\n        run {\n            1\n        }\n    }\n    val s = \"\"\"\n        x\n    \"\"\"\n}\n// one\n// two\nfun g(a: Int) { }\n";
    let got = folds(src);
    assert!(got.contains(&(0, 1, Some("imports".into()))), "{got:?}");
    assert!(got.contains(&(3, 5, Some("comment".into()))), "{got:?}");
    // The class folds up to the line before its closing brace; so does the function and the lambda.
    assert!(got.contains(&(6, 14, None)), "{got:?}");
    assert!(got.contains(&(7, 10, None)), "{got:?}");
    assert!(got.contains(&(8, 9, None)), "{got:?}");
    assert!(got.contains(&(12, 14, None)), "{got:?}");
    assert!(got.contains(&(16, 17, Some("comment".into()))), "{got:?}");
    // One line stays unfolded.
    assert!(!got.iter().any(|f| f.0 == 18), "{got:?}");
}

#[test]
fn selection_ranges_grow_from_the_identifier_to_the_file() {
    let src = "class K {\n    fun f(list: List<Int>) {\n        val n = list.size + 1\n    }\n}\n";
    let file = super::syntax::parse(src);
    let at = src.find("size").unwrap() + 1;
    let position = crate::text::position(src, at);
    let result = super::ranges::selection_ranges(&file, src, &[position]);
    let mut texts = Vec::new();
    let mut node = &result[0];
    let text = |range: &Value| {
        let (s, e) = (crate::text::offset(src, &range["start"]), crate::text::offset(src, &range["end"]));
        src[s..e].to_owned()
    };
    loop {
        texts.push(text(&node["range"]));
        if node["parent"].is_null() {
            break;
        }
        node = &node["parent"];
    }
    assert_eq!(texts[0], "size");
    assert_eq!(texts[1], "list.size");
    assert!(texts.contains(&"val n = list.size + 1".to_owned()), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("fun f(")), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("class K")), "{texts:?}");
    assert_eq!(texts.last().unwrap(), src);
    // Each range holds the one before it.
    assert!(texts.windows(2).all(|w| w[1].len() > w[0].len()), "{texts:?}");
}

// ----- implementations and hierarchies -----

/// `file:name` of each symbol.
fn names(an: &Analyzer, syms: &[super::index::SymId]) -> Vec<String> {
    let mut v: Vec<String> = syms
        .iter()
        .map(|&s| {
            let file = an.unit(s.unit_id()).path.file_name().unwrap().to_string_lossy().into_owned();
            let d = an.decl(s);
            let owner = an.file(s.unit_id()).owner_class(s.decl_id()).map(|c| an.file(s.unit_id()).decls[c].name.clone());
            format!("{file}:{}{}", owner.map(|o| format!("{o}.")).unwrap_or_default(), d.name)
        })
        .collect();
    v.sort();
    v
}

const SHAPES: &str = "package app\n\ninterface Sh$0ape {\n    fun area(): Double\n}\n\nclass Circle : Shape {\n    override fun area(): Double = 1.0\n}\n\nopen class Square : Shape {\n    override fun area(): Double = 2.0\n}\n\nclass Cube : Square() {\n    override fun area(): Double = 3.0\n}\n";

#[test]
fn finds_implementations_of_interfaces_and_members() {
    let f = fixture(&[("src/Shapes.kt", SHAPES)]);
    let (u, tok) = f.token();
    let an = f.an();
    let sym = features::targets_at(&an, u, tok).into_iter().find_map(|t| t.sym()).unwrap();
    assert_eq!(names(&an, &super::hierarchy::implementations(&an, sym)), ["Shapes.kt:Circle", "Shapes.kt:Cube", "Shapes.kt:Square"]);
    let area = an.decl(sym).members[0];
    let found = super::hierarchy::implementations(&an, u.sym(area));
    assert_eq!(names(&an, &found), ["Shapes.kt:Circle.area", "Shapes.kt:Cube.area", "Shapes.kt:Square.area"]);
}

#[test]
fn finds_actual_declarations_of_expect() {
    let f = fixture(&[
        ("src/Platform.kt", "package app\n\nexpect fun platform$0Name(): String\n"),
        ("src-jvm/Platform.jvm.kt", "package app\n\nactual fun platformName(): String = \"JVM\"\n"),
    ]);
    let (u, tok) = f.token();
    let an = f.an();
    let sym = features::targets_at(&an, u, tok).into_iter().find_map(|t| t.sym()).unwrap();
    assert_eq!(names(&an, &super::hierarchy::implementations(&an, sym)), ["Platform.jvm.kt:platformName"]);
}

#[test]
fn walks_the_type_hierarchy() {
    let f = fixture(&[("src/Shapes.kt", SHAPES)]);
    let (u, tok) = f.token();
    let an = f.an();
    let item = super::hierarchy::prepare_types(&an, u, tok)[0].clone();
    let subs = super::hierarchy::direct_subtypes(&an, &item);
    let names: Vec<&str> = subs.as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Circle", "Square"], "{subs}");
    let square = subs[1].clone();
    let supers = super::hierarchy::supertypes(&an, &square);
    assert_eq!(supers[0]["name"], "Shape", "{supers}");
    let cubes = super::hierarchy::direct_subtypes(&an, &square);
    assert_eq!(cubes[0]["name"], "Cube", "{cubes}");
}

#[test]
fn walks_the_call_hierarchy() {
    let f = fixture(&[(
        "src/Main.kt",
        "package app\n\nfun hel$0per(x: Int): Int = x\n\nfun middle(): Int = helper(1) + helper(2)\n\nfun top() {\n    middle()\n    helper(3)\n}\n",
    )]);
    let (u, tok) = f.token();
    let an = f.an();
    let item = super::hierarchy::prepare_calls(&an, u, tok)[0].clone();
    let incoming = super::hierarchy::incoming(&an, &item);
    let calls: Vec<(String, usize)> = incoming
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["from"]["name"].as_str().unwrap().to_owned(), c["fromRanges"].as_array().unwrap().len()))
        .collect();
    assert_eq!(calls, [("middle".to_owned(), 2), ("top".to_owned(), 1)], "{incoming}");
    let top = incoming[1]["from"].clone();
    let outgoing = super::hierarchy::outgoing(&an, &top);
    let to: Vec<&str> = outgoing.as_array().unwrap().iter().map(|c| c["to"]["name"].as_str().unwrap()).collect();
    assert_eq!(to, ["middle", "helper"], "{outgoing}");
}

#[test]
fn offers_the_members_of_data_classes() {
    let decl = "data class Point(val x: Int, val y: String)\nfun main() {\n    val p = Point(1, \"a\")\n";
    let f = fixture(&[("src/Main.kt", &format!("{decl}    p.$0\n}}\n"))]);
    let labels = f.labels();
    for name in ["copy", "component1", "component2", "toString", "equals", "hashCode", "x"] {
        assert!(has(&labels, name), "{name} in {labels:?}");
    }
    assert!(!has(&labels, "component3"), "{labels:?}");

    let f = fixture(&[("src/Main.kt", &format!("{decl}    p.co$0py(y = \"b\")\n}}\n"))]);
    let hover = f.hover();
    assert!(hover.contains("fun copy(x: Int = …, y: String = …): Point"), "{hover}");
    assert!(hover.contains("Generated by the compiler for `Point`"), "{hover}");
    assert_eq!(f.definition(), Some(("Main.kt".into(), "data class Point(val x: Int, val y: String)".into())));

    // The copy is a Point, and component2 is the second property.
    let f = fixture(&[("src/Main.kt", &format!("{decl}    p.copy().$0\n}}\n"))]);
    assert!(has(&f.labels(), "component2"), "{:?}", f.labels());
    let f = fixture(&[("src/Main.kt", &format!("{decl}    p.component2().$0\n}}\n"))]);
    assert!(has(&f.labels(), "length"), "{:?}", f.labels());

    // A member the class declares itself wins.
    let f = fixture(&[(
        "src/Main.kt",
        "data class P(val x: Int) {\n    override fun toString() = \"P\"\n}\nfun main() {\n    P(1).to$0String()\n}\n",
    )]);
    assert!(!f.hover().contains("Generated"), "{}", f.hover());
}

#[test]
fn offers_the_members_of_enum_classes() {
    let decl = "enum class Color { RED, GREEN }\nfun main() {\n";
    let f = fixture(&[("src/Main.kt", &format!("{decl}    Color.$0\n}}\n"))]);
    let labels = f.labels();
    for name in ["RED", "values", "valueOf", "entries"] {
        assert!(has(&labels, name), "{name} in {labels:?}");
    }
    let f = fixture(&[("src/Main.kt", &format!("{decl}    Color.RED.$0\n}}\n"))]);
    let labels = f.labels();
    assert!(has(&labels, "name") && has(&labels, "ordinal"), "{labels:?}");
    assert!(!has(&labels, "values") && !has(&labels, "valueOf"), "{labels:?}");

    let f = fixture(&[("src/Main.kt", &format!("{decl}    Color.val$0ueOf(\"RED\").$0\n}}\n"))]);
    assert!(f.hover().contains("fun valueOf(value: String): Color"), "{}", f.hover());
    assert_eq!(f.definition(), Some(("Main.kt".into(), "enum class Color { RED, GREEN }".into())));
    let f = fixture(&[("src/Main.kt", &format!("{decl}    Color.values().$0\n}}\n"))]);
    assert!(has(&f.labels(), "size"), "{:?}", f.labels());

    // Inside the enum, they are in scope by their plain names.
    let f = fixture(&[("src/Main.kt", "enum class Color { RED;\n    fun all() = val$0ues()\n}\n")]);
    assert!(f.hover().contains("fun values(): Array<Color>"), "{}", f.hover());
}

#[test]
fn generated_members_cannot_be_renamed() {
    let f = fixture(&[("src/Main.kt", "data class P(val x: Int)\nfun main() {\n    P(1).co$0py()\n}\n")]);
    let (u, tok) = f.token();
    let an = f.an();
    let sym = features::targets_at(&an, u, tok).into_iter().find_map(|t| t.sym()).expect("the generated member");
    assert!(an.decl(sym).generated);
    // Nothing is declared by its name, so nothing but the call is found.
    let refs = features::references(&an, &features::rename_group(&an, sym), true);
    assert_eq!(refs.len(), 1, "{refs:?}");
}

#[test]
fn converts_lambdas_to_functional_interfaces() {
    let decl = "fun interface Handler {\n    fun handle(n: Int): String\n}\nfun run(h: Handler) {}\n";
    let f = fixture(&[("src/Main.kt", &format!("{decl}fun main() {{\n    val h = Handler {{ n -> n.$0 }}\n}}\n"))]);
    assert!(has(&f.labels(), "toString"), "{:?}", f.labels());
    let f = fixture(&[("src/Main.kt", &format!("{decl}fun main() {{\n    val h = Handler {{ it.$0 }}\n}}\n"))]);
    assert!(has(&f.labels(), "toString"), "{:?}", f.labels());
    let f = fixture(&[("src/Main.kt", &format!("{decl}fun main() {{\n    run {{ n -> n.$0 }}\n}}\n"))]);
    assert!(has(&f.labels(), "toString"), "{:?}", f.labels());
    let f = fixture(&[("src/Main.kt", &format!("{decl}fun main() {{\n    run {{ i$0t }}\n}}\n"))]);
    assert!(f.hover().contains("it: Int"), "{}", f.hover());
}

#[test]
fn completes_invoke_on_function_values() {
    let f = fixture(&[("src/Main.kt", "fun main() {\n    val f: (Int, String) -> Boolean = { _, _ -> true }\n    f.$0\n}\n")]);
    let items = f.items();
    let invoke = items.iter().find(|i| i["label"] == "invoke").expect("invoke");
    assert_eq!(invoke["detail"], "operator fun invoke(p1: Int, p2: String): Boolean", "{invoke}");
}

#[test]
fn resolves_objects_in_when_branches() {
    let src = "sealed interface State\nobject Idle : State\nclass Busy : State\nfun f(s: State) = when (s) {\n    Id$0le -> 1\n    is Busy -> 2\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    assert!(f.hover().contains("object Idle"), "{}", f.hover());
    assert_eq!(f.definition(), Some(("Main.kt".into(), "object Idle : State".into())));
}

/// Replacing one file's entries gives the index a full build would, in the same order.
#[test]
fn replaces_a_file_in_the_index_like_a_rebuild() {
    use std::collections::{BTreeMap, HashMap};
    let unit = |path: &str, text: &str| Arc::new(Unit::new(PathBuf::from(path), text.to_owned(), Origin::Loose));
    let a = unit("a.kt", "package p\nclass A { class Inner }\nfun List<Int>.ext() {}\n");
    let b = unit("b.kt", "package p\nclass B\nfun top() {}\n");
    let c = unit("c.kt", "package p\nclass A\n");
    let mut index = Index::new(false, vec![a, b, c.clone()]);
    let b2 = unit("b.kt", "package p\nclass B2\nclass Inner2 { class Deep }\nfun List<String>.ext() {}\n");
    assert!(index.replace_unit(1, Arc::clone(&b2)));
    let fresh = Index::new(false, vec![index.units[0].clone(), b2, c]);
    let sorted = |m: &HashMap<String, Vec<super::index::SymId>>| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<BTreeMap<_, _>>();
    assert_eq!(sorted(&index.by_fq), sorted(&fresh.by_fq));
    assert_eq!(sorted(&index.by_name), sorted(&fresh.by_name));
    assert_eq!(sorted(&index.by_package), sorted(&fresh.by_package));
    assert_eq!(sorted(&index.extensions), sorted(&fresh.extensions));
    assert_eq!(index.names, fresh.names);
    // A new package needs a rebuild.
    assert!(!index.replace_unit(1, unit("b.kt", "package q\nclass B\n")));
}

/// Times the operations that scale with the size of a project: building the index, replacing one
/// file in it, looking up scopes in a large file, and searching for references. `FILES` (default
/// 2000) sets the number of files; `KOTLIN_SOURCES`, when set, adds the extracted standard library
/// sources and reports how much memory their text and declarations take.
/// `cargo test -p kargo-lsp --release -- --ignored --nocapture scales_with_the_project`.
#[test]
#[ignore]
fn scales_with_the_project() {
    use std::time::Instant;
    let files: usize = std::env::var("FILES").ok().and_then(|n| n.parse().ok()).unwrap_or(2000);
    let source = |i: usize| {
        let mut text = format!("package p{}\n\nclass C{i} {{\n", i % 20);
        for m in 0..20 {
            text.push_str(&format!("    fun m{m}(x: Int): Int {{\n        val y = x + {m}\n        return helper{}(y)\n    }}\n", i % 50));
        }
        text.push_str("}\n");
        if i < 50 {
            text.push_str(&format!("fun helper{i}(a: Int): Int = a\n"));
        }
        text
    };
    let unit = |i: usize, text: String| Arc::new(Unit::new(PathBuf::from(format!("/p/src/F{i:05}.kt")), text, Origin::Loose));
    let t = Instant::now();
    let units: Vec<Arc<Unit>> = (0..files).map(|i| unit(i, source(i))).collect();
    println!("parse {files} files: {:?}", t.elapsed());
    let t = Instant::now();
    let mut index = Index::new(false, units.clone());
    println!("build the index: {:?}", t.elapsed());
    let t = Instant::now();
    let rounds = 100;
    for r in 0..rounds {
        let i = (r * 7) % files;
        assert!(index.replace_unit(i as u32, unit(i, source(i))));
    }
    println!("replace a file (avg of {rounds}, with the parse): {:?}", t.elapsed() / rounds as u32);

    let big: String = (0..2000).map(|i| format!("fun f{i}(a: Int): Int {{\n    val b = a + {i}\n    return b\n}}\n")).collect();
    let big = unit(0, big);
    let t = Instant::now();
    for offset in (0..big.text.len()).step_by(7) {
        big.file.scope_at(offset);
    }
    println!("scope_at at every 7th offset of a {} line file: {:?}", big.text.lines().count(), t.elapsed());

    let lib = Index::new(true, Vec::new());
    let packages = Packages { visible: vec![vec![0]], platforms: vec![Platforms::ALL] };
    let an = Analyzer::new(&lib, &index, &packages);
    let helper = *index.by_name.get("helper7").and_then(|s| s.first()).expect("helper7");
    let t = Instant::now();
    let refs = features::references(&an, &features::rename_group(&an, helper), true);
    println!("references of helper7: {} in {:?}", refs.len(), t.elapsed());

    if let Ok(dir) = std::env::var("KOTLIN_SOURCES") {
        let paths = kargo_util::fs::collect_files(std::path::Path::new(&dir), &["kt"]);
        let t = Instant::now();
        let origin = Origin::Library { name: "stdlib".into(), main: Platforms::ALL, test: Platforms::NONE };
        let units: Vec<Arc<Unit>> =
            paths.iter().filter_map(|p| Some(Arc::new(Unit::new(p.clone(), std::fs::read_to_string(p).ok()?, origin.clone())))).collect();
        let text: usize = units.iter().map(|u| u.text.len()).sum();
        let decls: usize = units.iter().map(|u| u.file.decls.len()).sum();
        let tokens: usize = units.iter().map(|u| u.file.lexed.tokens.len()).sum();
        println!(
            "library: {} files, {} KiB of text, {decls} declarations, {tokens} tokens, parsed in {:?}",
            units.len(),
            text / 1024,
            t.elapsed()
        );
        let token_kib = tokens * std::mem::size_of::<super::lexer::Token>() / 1024;
        let decl_kib = decls * std::mem::size_of::<super::syntax::Decl>() / 1024;
        println!("  memory: text {} KiB, tokens {token_kib} KiB, declarations {decl_kib} KiB (shallow)", text / 1024);
        let t = Instant::now();
        let blobs: Vec<Vec<u8>> = units.iter().map(|u| super::parsed::encode(&u.file)).collect();
        println!("  encode: {:?}, {} KiB stored", t.elapsed(), blobs.iter().map(Vec::len).sum::<usize>() / 1024);
        let t = Instant::now();
        let decoded: usize = blobs.iter().filter_map(|b| super::parsed::decode(b)).map(|f| f.decls.len()).sum();
        println!("  decode: {:?} ({decoded} declarations)", t.elapsed());
    }
}

#[test]
fn serializable_classes_get_serializer() {
    let decl = "@Serializable\nclass Person(val name: String)\nfun main() {\n";
    let f = fixture(&[("src/Main.kt", &format!("{decl}    Person.$0\n}}\n"))]);
    assert!(has(&f.labels(), "serializer"), "{:?}", f.labels());
    let f = fixture(&[("src/Main.kt", &format!("{decl}    Person.seri$0alizer()\n}}\n"))]);
    assert!(f.hover().contains("fun serializer(): kotlinx.serialization.KSerializer<Person>"), "{}", f.hover());
    assert_eq!(f.definition().map(|d| d.0), Some("Main.kt".into()));
    // Without the annotation there is none.
    let f = fixture(&[("src/Main.kt", "class Person(val name: String)\nfun main() {\n    Person.$0\n}\n")]);
    assert!(!has(&f.labels(), "serializer"), "{:?}", f.labels());
    // A generic class takes the serializers of its type arguments.
    let f = fixture(&[("src/Main.kt", "@Serializable\nclass Box<T>(val v: T)\nfun main() {\n    Box.seri$0alizer()\n}\n")]);
    assert!(f.hover().contains("typeSerial0: kotlinx.serialization.KSerializer<T>"), "{}", f.hover());
}

#[test]
fn composable_function_types_keep_their_parameters() {
    let src = "annotation class Composable\nfun Content(body: @Composable (Int) -> Unit) {}\nfun main() {\n    Content { n -> n.$0 }\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    assert!(has(&f.labels(), "toString"), "{:?}", f.labels());
    let src = "annotation class Composable\nfun Content(body: @Composable () -> Unit) {}\nfun main() {\n    Content { 1.$0 }\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    assert!(has(&f.labels(), "toString"), "{:?}", f.labels());
}

#[test]
fn internal_declarations_stay_in_their_package() {
    let lib = ("lib/src/Lib.kt", "package lib\n\ninternal fun hidden() {}\nfun shown() {}\n");
    let f = fixture(&[lib, ("src/Main.kt", "package app\n\nimport lib.*\n\nfun main() {\n    $0\n}\n")]);
    let labels = f.labels();
    assert!(has(&labels, "shown") && !has(&labels, "hidden"), "{labels:?}");
    // The package's own source sets still see it.
    let f = fixture(&[
        ("lib/src/Lib.kt", "package lib\n\ninternal fun hidden() {}\n"),
        ("lib/tests/LibTest.kt", "package lib\n\nfun t() {\n    hid$0\n}\n"),
    ]);
    assert!(has(&f.labels(), "hidden"), "{:?}", f.labels());
}

const LOMBOK_USER: &str = "package app;\n\n@Data\n@Builder\npublic class User {\n    private final String name;\n    private int age;\n    private boolean active;\n}\n";

#[test]
fn lombok_generates_getters_setters_and_constructors() {
    let user = ("src/User.java", LOMBOK_USER);
    let f = fixture(&[user, ("src/Main.kt", "package app\n\nfun main(u: User) {\n    u.$0\n}\n")]);
    let labels = f.labels();
    for name in ["getName", "getAge", "setAge", "isActive", "setActive"] {
        assert!(has(&labels, name), "{name} in {labels:?}");
    }
    assert!(!has(&labels, "setName"), "a final field has no setter: {labels:?}");
    let f = fixture(&[user, ("src/Main.kt", "package app\n\nfun main(u: User) {\n    u.getA$0ge()\n}\n")]);
    assert!(f.hover().contains("Generated by Lombok for `User`"), "{}", f.hover());
    assert_eq!(f.definition().map(|d| d.0), Some("User.java".into()));
    // @Data takes the final fields as constructor arguments.
    let f = fixture(&[user, ("src/Main.kt", "package app\n\nfun main() {\n    User(\"a\").getN$0ame()\n}\n")]);
    assert!(f.hover().contains("getName"), "{}", f.hover());
    // An accessor the class declares itself is not duplicated.
    let own = "package app;\n\n@Getter\npublic class P {\n    private int x;\n    public int getX() { return 1; }\n}\n";
    let f = fixture(&[("src/P.java", own), ("src/Main.kt", "package app\n\nfun main(p: P) {\n    p.$0\n}\n")]);
    assert_eq!(f.labels().iter().filter(|l| l.starts_with("getX")).count(), 1, "{:?}", f.labels());
    // A field annotated on its own.
    let field = "package app;\n\npublic class Q {\n    @Setter private String s;\n    private int t;\n}\n";
    let f = fixture(&[("src/Q.java", field), ("src/Main.kt", "package app\n\nfun main(q: Q) {\n    q.$0\n}\n")]);
    assert!(has(&f.labels(), "setS") && !has(&f.labels(), "setT") && !has(&f.labels(), "getS"), "{:?}", f.labels());
}

#[test]
fn lombok_builders_chain() {
    let user = ("src/User.java", LOMBOK_USER);
    let f = fixture(&[user, ("src/Main.kt", "package app\n\nfun main() {\n    User.builder().$0\n}\n")]);
    let labels = f.labels();
    assert!(has(&labels, "name") && has(&labels, "age") && has(&labels, "build"), "{labels:?}");
    let f = fixture(&[user, ("src/Main.kt", "package app\n\nfun main() {\n    User.builder().name(\"a\").build().$0\n}\n")]);
    assert!(has(&f.labels(), "getName"), "{:?}", f.labels());
}

#[test]
fn spring_classes_are_marked_open() {
    let src = "@Service\nclass Greeter\nclass Plain\nfun main() {\n    Gree$0ter()\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    assert!(f.hover().contains("Open for Spring"), "{}", f.hover());
    let f = fixture(&[("src/Main.kt", "class Plain\nfun main() {\n    Pla$0in()\n}\n")]);
    assert!(!f.hover().contains("Open for Spring"), "{}", f.hover());
}

fn item<'a>(items: &'a [Value], label: &str) -> &'a Value {
    items
        .iter()
        .find(|i| i["label"] == label)
        .unwrap_or_else(|| panic!("{label} in {:?}", items.iter().map(|i| &i["label"]).collect::<Vec<_>>()))
}

fn first_labels(items: &[Value], n: usize) -> Vec<String> {
    let mut sorted: Vec<&Value> = items.iter().collect();
    sorted.sort_by(|a, b| a["sortText"].as_str().cmp(&b["sortText"].as_str()));
    sorted.iter().take(n).map(|i| i["label"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn completion_ranks_values_of_the_expected_type_first() {
    let src = "fun main() {\n    val count = 1\n    val name = \"a\"\n    val x: String = $0\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    let items = f.items();
    assert_eq!(first_labels(&items, 1), vec!["name"], "{:?}", first_labels(&items, 5));

    let src = "fun take(n: Int) {}\nfun main() {\n    val count = 1\n    val name = \"a\"\n    take($0)\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    assert_eq!(first_labels(&f.items(), 1), vec!["count"]);

    let src = "fun f(): String {\n    val count = 1\n    val name = \"a\"\n    return $0\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    assert_eq!(first_labels(&f.items(), 1), vec!["name"]);
}

#[test]
fn completion_offers_named_arguments() {
    let src = "fun greet(name: String, times: Int = 1) {}\nfun main() {\n    greet($0)\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    let items = f.items();
    let arg = item(&items, "name =");
    assert_eq!(arg["textEdit"]["newText"], "name = ");
    item(&items, "times =");

    let src = "fun greet(name: String, times: Int = 1) {}\nfun main() {\n    greet(\"a\", $0)\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    let items = f.items();
    item(&items, "times =");
    assert!(!items.iter().any(|i| i["label"] == "name ="));
}

#[test]
fn completion_stubs_overrides() {
    let src = "interface Shape {\n    val sides: Int\n    fun area(scale: Double): Double\n    fun describe(): String = \"shape\"\n}\nclass Square : Shape {\n    ar$0\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    let items = f.items();
    let area = item(&items, "area");
    assert_eq!(area["textEdit"]["newText"], "override fun area(scale: Double): Double {\n        TODO(\"Not yet implemented\")\n    }");

    let src = src.replace("ar$0", "override $0");
    let f = fixture(&[("src/Main.kt", &src)]);
    let items = f.items();
    assert!(item(&items, "sides")["textEdit"]["newText"].as_str().unwrap().starts_with("val sides: Int"));
    let describe = item(&items, "describe")["textEdit"]["newText"].as_str().unwrap().to_owned();
    assert!(describe.contains("return super.describe()"), "{describe}");
    // Not a stub for a member that is already overridden.
    let src = src.replace("override $0", "override fun area(scale: Double) = 1.0\n    override $0");
    let f = fixture(&[("src/Main.kt", &src)]);
    assert!(!f.items().iter().any(|i| i["label"] == "area"));
}

#[test]
fn completion_offers_only_the_keywords_that_fit() {
    let kw = |src: &str| -> Vec<String> {
        let f = fixture(&[("src/Main.kt", src)]);
        f.items().iter().filter(|i| i["kind"] == 14).map(|i| i["label"].as_str().unwrap().to_owned()).collect()
    };
    // In an expression: no declarations.
    let k = kw("fun main() {\n    val x = $0\n}\n");
    assert!(k.contains(&"if".to_owned()) && !k.contains(&"class".to_owned()) && !k.contains(&"val".to_owned()), "{k:?}");
    // `else` only after an `if`.
    let k = kw("fun main(a: Boolean) {\n    if (a) {\n    } $0\n}\n");
    assert!(k.contains(&"else".to_owned()), "{k:?}");
    let k = kw("fun main() {\n    val x = 1\n    $0\n}\n");
    assert!(!k.contains(&"else".to_owned()), "{k:?}");
    // In a class body: declarations, not statements.
    let k = kw("class A {\n    $0\n}\n");
    assert!(k.contains(&"fun".to_owned()) && k.contains(&"override".to_owned()) && !k.contains(&"while".to_owned()), "{k:?}");
    // `is` in a `when` branch.
    let k = kw("fun f(x: Any) = when (x) {\n    $0\n}\n");
    assert!(k.contains(&"is".to_owned()) && k.contains(&"else".to_owned()), "{k:?}");
    let k = kw("fun main() {\n    val x = 1\n    $0\n}\n");
    assert!(!k.contains(&"is".to_owned()), "{k:?}");
}

#[test]
fn completion_inserts_trailing_lambdas() {
    let src =
        "fun <T> eachIndexed(action: (Int, T) -> Unit) {}\nfun repeatIt(times: Int, body: () -> Unit) {}\nfun main() {\n    eachI$0\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    let items = f.items();
    assert_eq!(item(&items, "eachIndexed")["textEdit"]["newText"], "eachIndexed { ${1:int}, ${2:t} -> $0 }");

    let f = fixture(&[("src/Main.kt", &src.replace("eachI$0", "repeatI$0"))]);
    let items = f.items();
    assert_eq!(item(&items, "repeatIt")["textEdit"]["newText"], "repeatIt(${1:times}) { $0 }");
}

#[test]
fn completion_names_lambda_parameters_as_written() {
    let src = "fun eachIndexed(action: (index: Int, value: String) -> Unit) {}\nfun main() {\n    eachI$0\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    let items = f.items();
    assert_eq!(item(&items, "eachIndexed")["textEdit"]["newText"], "eachIndexed { ${1:index}, ${2:value} -> $0 }");
}

#[test]
fn completion_puts_recently_inserted_names_first() {
    let src = "fun main() {\n    val alpha = 1\n    val beta = 2\n    $0\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    let (u, offset) = f.cursor();
    let mut list = features::completion(&f.an(), u, offset, CompletionOptions { snippets: true, ..Default::default() }, 0);
    assert_eq!(first_labels(list["items"].as_array().unwrap(), 2), vec!["alpha", "beta"]);
    let recent = std::collections::HashMap::from([("beta".to_owned(), 1u64)]);
    features::rank_recent(&mut list, &recent);
    assert_eq!(first_labels(list["items"].as_array().unwrap(), 1), vec!["beta"]);
    assert_eq!(features::words("val x = fooBar(a, 1)").collect::<Vec<_>>(), vec!["val", "fooBar"]);
}

#[test]
fn completion_offers_postfix_templates() {
    let src = "fun main() {\n    val ready = true\n    ready.$0\n}\n";
    let f = fixture(&[("src/Main.kt", src)]);
    let items = f.items();
    let val = item(&items, "val");
    assert_eq!(val["textEdit"]["newText"], "val ${1:name} = ready");
    assert_eq!(val["filterText"], "ready.val");
    assert_eq!(items.iter().find(|i| i["label"] == "not" && i["kind"] == 15).unwrap()["textEdit"]["newText"], "!ready");
    assert!(item(&items, "if")["textEdit"]["newText"].as_str().unwrap().starts_with("if (ready) {"));
    // Not inside an expression.
    let f = fixture(&[("src/Main.kt", "fun main() {\n    val ready = true\n    val x = ready.$0\n}\n")]);
    let items = f.items();
    assert!(!items.iter().any(|i| i["label"] == "val") && items.iter().any(|i| i["label"] == "not" && i["kind"] == 15));
}

// ----- rename beyond identifiers -----

mod renaming {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::*;
    use crate::kotlin::rename::{self, Env};
    use crate::text;

    fn env(manifest: Option<&std::path::Path>) -> Env {
        Env {
            roots: vec![PathBuf::from("/project/src")],
            manifests: manifest.map(|m| vec![m.to_owned()]).unwrap_or_default(),
            versions: [(PathBuf::from("/project/src/Main.kt"), 7)].into(),
            document_changes: true,
            resource_renames: true,
            rename_files: true,
            move_directories: true,
        }
    }

    /// The edit of renaming what is at the cursor.
    fn edit(f: &Fixture, to: &str, env: &Env) -> Result<Value, String> {
        let (unit, offset) = f.cursor();
        let an = f.an();
        let found = rename::subject(&an, unit, offset)?;
        rename::rename(&an, env, &found, to, &features::Control::default())
    }

    fn name(uri: &str) -> String {
        uri.rsplit('/').next().unwrap().to_owned()
    }

    /// The files an edit changes, by file name, with the edits applied; and the renames.
    fn apply(f: &Fixture, edit: &Value) -> (BTreeMap<String, String>, Vec<(String, String)>) {
        let mut files = BTreeMap::new();
        let mut renames = Vec::new();
        for change in edit["documentChanges"].as_array().expect("documentChanges") {
            if change["kind"] == "rename" {
                renames.push((name(change["oldUri"].as_str().unwrap()), name(change["newUri"].as_str().unwrap())));
                continue;
            }
            let uri = change["textDocument"]["uri"].as_str().unwrap();
            let mut text = match f.proj.units.iter().find(|u| text::path_to_uri(&u.path) == uri) {
                Some(unit) => unit.text.clone(),
                None => std::fs::read_to_string(text::uri_to_path(uri).unwrap()).unwrap(),
            };
            let mut edits = change["edits"].as_array().unwrap().clone();
            edits.sort_by_key(|e| std::cmp::Reverse((e["range"]["start"]["line"].as_u64(), e["range"]["start"]["character"].as_u64())));
            for e in edits {
                let (start, end) = (text::offset(&text, &e["range"]["start"]), text::offset(&text, &e["range"]["end"]));
                text.replace_range(start..end, e["newText"].as_str().unwrap());
            }
            files.insert(name(uri), text);
        }
        (files, renames)
    }

    /// Renames what is at the cursor and applies the edit.
    fn renamed(f: &Fixture, to: &str) -> BTreeMap<String, String> {
        apply(f, &edit(f, to, &env(None)).unwrap()).0
    }

    #[test]
    fn renames_the_file_of_a_class_with_the_class() {
        let f = fixture(&[
            ("src/User.kt", "package app\n\nclass Us$0er(val name: String)\n"),
            ("src/Main.kt", "package app\n\nfun main() {\n    println(User(\"a\").name)\n}\n"),
        ]);
        let edit = edit(&f, "Person", &env(None)).unwrap();
        let (files, renames) = apply(&f, &edit);
        assert_eq!(renames, [("User.kt".to_owned(), "Person.kt".to_owned())]);
        assert!(files["User.kt"].contains("class Person("), "{files:?}");
        assert!(files["Main.kt"].contains("Person(\"a\")"), "{files:?}");
        // An open document is edited at the version the editor has; the others at none.
        let versions: Vec<(String, Value)> = edit["documentChanges"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c.get("textDocument").is_some())
            .map(|c| (name(c["textDocument"]["uri"].as_str().unwrap()), c["textDocument"]["version"].clone()))
            .collect();
        assert!(versions.contains(&("Main.kt".to_owned(), json!(7))), "{versions:?}");
        assert!(versions.contains(&("User.kt".to_owned(), Value::Null)), "{versions:?}");

        // Without `rename` operations only the text changes.
        let mut plain = env(None);
        plain.resource_renames = false;
        let (_, renames) = apply(&f, &super::renaming::edit(&f, "Person", &plain).unwrap());
        assert!(renames.is_empty());
        let mut changes = env(None);
        changes.document_changes = false;
        let edit = super::renaming::edit(&f, "Person", &changes).unwrap();
        assert!(edit["changes"].is_object() && edit.get("documentChanges").is_none(), "{edit}");
        // A file that isn't named after the class stays.
        let f = fixture(&[("src/Models.kt", "package app\n\nclass Us$0er\n")]);
        let (_, renames) = apply(&f, &super::renaming::edit(&f, "Person", &env(None)).unwrap());
        assert!(renames.is_empty());
    }

    #[test]
    fn renames_a_file_through_will_rename() {
        let f = fixture(&[
            ("src/User.kt", "package app\n\nclass User(val name: String)\n"),
            ("src/Main.kt", "package app\n\nfun main() {\n    println(User(\"a\").name)\n}\n"),
        ]);
        let files = [(PathBuf::from("/project/src/User.kt"), PathBuf::from("/project/src/Person.kt"))];
        let edit = rename::will_rename(&f.an(), &env(None), &files, &features::Control::default()).unwrap();
        let (changed, renames) = apply(&f, &edit);
        assert!(renames.is_empty());
        assert!(changed["User.kt"].contains("class Person("), "{changed:?}");
        assert!(changed["Main.kt"].contains("Person(\"a\")"), "{changed:?}");
        // A file that names no class changes nothing.
        let other = [(PathBuf::from("/project/src/Main.kt"), PathBuf::from("/project/src/Start.kt"))];
        assert!(rename::will_rename(&f.an(), &env(None), &other, &features::Control::default()).unwrap().is_null());
    }

    #[test]
    fn renames_packages_everywhere() {
        let f = fixture(&[
            ("src/model/User.kt", "package app.mo$0del\n\n/** See [app.model.User]. */\nclass User\n"),
            ("src/Main.kt", "package app\n\nimport app.model.User\n\nfun main() {\n    val u: app.model.User = User()\n}\n"),
            ("src/Other.kt", "package app.model.deep\n\nimport app.model.*\n\nval x = 1\n"),
        ]);
        let files = renamed(&f, "domain");
        assert!(files["User.kt"].starts_with("package app.domain\n"), "{files:?}");
        assert!(files["User.kt"].contains("[app.domain.User]"), "{files:?}");
        assert!(files["Main.kt"].contains("import app.domain.User"), "{files:?}");
        assert!(files["Main.kt"].contains("val u: app.domain.User"), "{files:?}");
        assert!(files["Other.kt"].starts_with("package app.domain.deep\n"), "{files:?}");
        assert!(files["Other.kt"].contains("import app.domain.*"), "{files:?}");
        // A package that exists is refused.
        let f = fixture(&[("src/A.kt", "package app.o$0ne\n\nval a = 1\n"), ("src/B.kt", "package app.two\n\nval b = 1\n")]);
        assert!(edit(&f, "two", &env(None)).is_err());
    }

    #[test]
    fn updates_the_entry_points_in_kargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("kargo.toml");
        std::fs::write(
            &manifest,
            "[package]\nname = \"app\"\n\n[target.jvm]\nmain-class = \"app.MainKt\"\n\n[target.native]\nentry-point = \"app.main\"\n\n[[bin]]\nname = \"tool\"\nmain = \"app.Tool\"\n",
        )
        .unwrap();
        let env = env(Some(&manifest));
        let toml_after = |f: &Fixture, to: &str| -> String {
            let (files, _) = apply(f, &edit(f, to, &env).unwrap());
            files["kargo.toml"].clone()
        };
        // The function `main`.
        let f = fixture(&[("src/Main.kt", "package app\n\nfun ma$0in() {}\n")]);
        let out = toml_after(&f, "start");
        assert!(out.contains("entry-point = \"app.start\""), "{out}");
        assert!(out.contains("main-class = \"app.MainKt\""), "{out}");
        // A class named by `[[bin]]`.
        let f = fixture(&[("src/Tool.kt", "package app\n\nclass To$0ol\n")]);
        let out = toml_after(&f, "Cli");
        assert!(out.contains("main = \"app.Cli\""), "{out}");
        // The package.
        let f = fixture(&[("src/Main.kt", "package a$0pp\n\nfun main() {}\n")]);
        let out = toml_after(&f, "demo");
        assert!(out.contains("main-class = \"demo.MainKt\""), "{out}");
        assert!(out.contains("entry-point = \"demo.main\""), "{out}");
        assert!(out.contains("main = \"demo.Tool\""), "{out}");
        // The file: `Main.kt` compiles to `MainKt`.
        let f = fixture(&[("src/Main.kt", "package app\n\nfun main() {}\n")]);
        let files = [(PathBuf::from("/project/src/Main.kt"), PathBuf::from("/project/src/Start.kt"))];
        let edit = rename::will_rename(&f.an(), &env, &files, &features::Control::default()).unwrap();
        let (changed, _) = apply(&f, &edit);
        assert!(changed["kargo.toml"].contains("main-class = \"app.StartKt\""), "{changed:?}");
    }

    #[test]
    fn renames_kdoc_links_and_tags() {
        let src = "package app\n\n/**\n * A user, see [User.name] and [name].\n * @param name the name\n * @property name again\n * @see User\n */\nclass User(val na$0me: String)\n\n/** Uses [User] and [User.name]. */\nfun use(u: User) = u.name\n";
        let f = fixture(&[("src/User.kt", src)]);
        let files = renamed(&f, "title");
        let out = &files["User.kt"];
        assert!(out.contains("[User.title] and [title]"), "{out}");
        assert!(out.contains("@param title the name"), "{out}");
        assert!(out.contains("@property title again"), "{out}");
        assert!(out.contains("[User] and [User.title]"), "{out}");
        assert!(out.contains("@see User"), "{out}");
        assert!(out.contains("u.title"), "{out}");

        // Renaming the class changes its links and `@see`.
        let class = src.replace("class User(val na$0me", "class Us$0er(val name");
        let f = fixture(&[("src/User.kt", &class)]);
        let files = renamed(&f, "Person");
        let out = &files["User.kt"];
        assert!(out.contains("[Person.name]") && out.contains("@see Person") && out.contains("[Person] and"), "{out}");

        // A function's parameters.
        let f = fixture(&[("src/F.kt", "/**\n * @param count how many\n */\nfun repeat(cou$0nt: Int) = count\n")]);
        let files = renamed(&f, "times");
        assert!(files["F.kt"].contains("@param times how many") && files["F.kt"].contains("repeat(times: Int) = times"), "{files:?}");

        // The bracket form of the tags, `@sample`, and brackets in code.
        let src = "package app

/**
 * Use `a[count]` and [`count`].
 * @param[count] how many
 * @sample app.repeat
 */
fun repeat(cou$0nt: Int) = count
";
        let f = fixture(&[("src/F.kt", src)]);
        let out = renamed(&f, "times")["F.kt"].clone();
        assert!(out.contains("`a[count]`") && out.contains("and [times]") && out.contains("@param[times] how many"), "{out}");
        let src = "package app

/**
 * @sample app.re$0peat
 */
fun use() = repeat()

fun repeat() = 1
";
        let f = fixture(&[("src/F.kt", src)]);
        let out = renamed(&f, "again")["F.kt"].clone();
        assert!(out.contains("@sample app.again") && out.contains("fun again() = 1") && out.contains("= again()"), "{out}");

        // Starting inside the comment.
        let f = fixture(&[("src/User.kt", "class User\n\n/** Makes a [Us$0er]. */\nfun make() = User()\n")]);
        let files = renamed(&f, "Person");
        assert!(
            files["User.kt"].contains("class Person") && files["User.kt"].contains("[Person]") && files["User.kt"].contains("= Person()"),
            "{files:?}"
        );
    }

    #[test]
    fn renames_type_parameters() {
        let src = "/**\n * @param T the element\n * Holds a [T].\n */\nclass Box<T$0>(val value: T) {\n    fun <T> other(x: T): T = x\n    fun get(): T = value\n    fun copy(): Box<T> = Box(value)\n}\n";
        let f = fixture(&[("src/Box.kt", src)]);
        let files = renamed(&f, "Item");
        let out = &files["Box.kt"];
        assert!(out.contains("class Box<Item>(val value: Item)"), "{out}");
        assert!(out.contains("fun get(): Item = value") && out.contains("Box<Item> = Box(value)"), "{out}");
        assert!(out.contains("fun <T> other(x: T): T = x"), "the inner T hides the class's: {out}");
        assert!(out.contains("@param Item the element") && out.contains("Holds a [Item]"), "{out}");

        // From a use.
        let f = fixture(&[("src/Id.kt", "fun <T> id(x: T$0): T = x\n")]);
        assert_eq!(renamed(&f, "A")["Id.kt"], "fun <A> id(x: A): A = x\n");
    }

    #[test]
    fn renames_labels() {
        let src = "fun main() {\n    outer$0@ for (i in 1..3) {\n        for (j in 1..3) {\n            if (j == 2) continue@outer\n            if (i == 3) break@outer\n        }\n    }\n    outer@ for (k in 1..2) {\n        break@outer\n    }\n}\n";
        let f = fixture(&[("src/Main.kt", src)]);
        let files = renamed(&f, "rows");
        let out = &files["Main.kt"];
        assert!(out.contains("rows@ for (i") && out.contains("continue@rows") && out.contains("break@rows"), "{out}");
        assert!(out.contains("outer@ for (k") && out.contains("break@outer"), "the second loop's label stays: {out}");

        // From a reference, and a label on a lambda.
        let src = "fun main() {\n    listOf(1).forEach loop@{\n        if (it == 1) return@lo$0op\n    }\n}\n";
        let f = fixture(&[("src/Main.kt", src)]);
        let files = renamed(&f, "each");
        assert!(files["Main.kt"].contains("forEach each@{") && files["Main.kt"].contains("return@each"), "{files:?}");
    }

    #[test]
    fn renaming_a_function_renames_the_labels_that_name_it() {
        let src =
            "fun run$0Twice(block: () -> Unit) { block(); block() }\n\nfun main() {\n    runTwice {\n        return@runTwice\n    }\n}\n";
        let f = fixture(&[("src/Main.kt", src)]);
        let files = renamed(&f, "twice");
        assert!(files["Main.kt"].contains("return@twice") && files["Main.kt"].contains("    twice {"), "{files:?}");
    }
}

#[test]
fn completes_kdoc_tags_names_and_links() {
    // The tags.
    let f = fixture(&[("src/F.kt", "/**\n * Repeats.\n * @$0\n */\nfun repeat(count: Int) = count\n")]);
    let labels = f.labels();
    for tag in [
        "@param",
        "@return",
        "@receiver",
        "@constructor",
        "@property",
        "@throws",
        "@exception",
        "@sample",
        "@see",
        "@author",
        "@since",
        "@suppress",
    ] {
        assert!(labels.contains(&tag.to_owned()), "{tag} in {labels:?}");
    }
    let item = f.items().into_iter().find(|i| i["label"] == "@param").unwrap();
    assert_eq!(item["textEdit"]["newText"], "param ${1:name} ${2:description}");

    // Parameters and type parameters after `@param`.
    let f = fixture(&[("src/F.kt", "/**\n * @param $0\n */\nfun <T> repeat(count: Int, times: T) = count\n")]);
    assert_eq!(f.labels(), ["count", "times", "T"]);
    let f = fixture(&[("src/F.kt", "/**\n * @param co$0\n */\nfun <T> repeat(count: Int, times: T) = count\n")]);
    assert!(f.labels().contains(&"count".to_owned()));

    // Links: the file's declarations, then the members after a dot.
    let src = "package app\n\nclass User(val name: String) {\n    fun greet() {}\n}\n\n/** See [Us$0] */\nfun make() = User(\"a\")\n";
    let f = fixture(&[("src/User.kt", src)]);
    assert!(f.labels().contains(&"User".to_owned()), "{:?}", f.labels());
    let src = "package app\n\nclass User(val name: String) {\n    fun greet() {}\n}\n\n/** See [User.$0] */\nfun make() = User(\"a\")\n";
    let f = fixture(&[("src/User.kt", src)]);
    let labels = f.labels();
    assert!(labels.contains(&"name".to_owned()) && labels.contains(&"greet".to_owned()), "{labels:?}");
    let src = "package app\n\nclass User\n\n/**\n * @see User$0\n */\nfun make() = User()\n";
    let f = fixture(&[("src/User.kt", src)]);
    assert!(f.labels().contains(&"User".to_owned()));

    // Plain comment text and code get nothing.
    let f = fixture(&[("src/F.kt", "/**\n * Some text $0\n */\nfun f() = 1\n")]);
    assert!(f.labels().is_empty());
    let f = fixture(&[("src/F.kt", "// @$0\nfun f() = 1\n")]);
    assert!(f.labels().is_empty());
}

#[test]
fn completes_the_exceptions_the_body_throws() {
    let src = "/**\n * Checks.\n * @throws $0\n */\nfun check(n: Int) {\n    if (n < 0) throw IllegalArgumentException(\"negative\")\n    if (n > 9) throw IllegalStateException()\n    if (n == 5) throw n\n}\n";
    let f = fixture(&[("src/F.kt", src)]);
    let items = f.items();
    assert_eq!(first_labels(&items, 2), ["IllegalArgumentException", "IllegalStateException"]);
    assert_eq!(item(&items, "IllegalArgumentException")["detail"], "thrown in the body");
    // A rethrown variable is not an exception the body names: it is offered only as a parameter.
    assert_ne!(item(&items, "n")["detail"], "thrown in the body");
    let f = fixture(&[("src/F.kt", "/**\n * @see $0\n */\nfun check(n: Int) {\n    throw IllegalStateException()\n}\n")]);
    assert!(f.items().iter().all(|i| i["detail"] != "thrown in the body"));
}

#[test]
fn hints_kdoc_names_that_resolve_to_nothing() {
    let src = "package app\n\nclass User\n\n/**\n * Makes a [User] and a [Missing].\n * @param count how many\n * @see app.Nope\n */\nfun make(size: Int) = User()\n";
    let f = fixture(&[("src/app/Main.kt", src)]);
    let problems = super::kdoc::problems(&f.an(), UnitId { lib: false, unit: 0 });
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();
    assert_eq!(problems.len(), 3, "{messages:?}");
    assert!(messages.contains(&"`count` is not a parameter or property of this declaration"), "{messages:?}");
    assert!(messages.contains(&"unresolved reference `app.Nope`") && messages.contains(&"unresolved reference `Missing`"), "{messages:?}");
    assert!(problems.iter().all(|p| p.severity == Severity::Hint));
    let missing = problems.iter().find(|p| p.message.ends_with("`Missing`")).unwrap();
    assert_eq!(&f.proj.units[0].text[missing.span.clone()], "Missing");
}

#[test]
fn navigates_the_names_in_kdoc_comments() {
    let src = "package app\n\nclass User(val name: String)\n\n/** Makes a [User] with its [User.name]. */\nfun make() = User(\"a\")\n";
    let f = fixture(&[("src/app/User.kt", src)]);
    let an = f.an();
    let unit = UnitId { lib: false, unit: 0 };
    let text = &f.proj.units[0].text;

    let link = text.find("[User]").unwrap() + 2;
    let (span, targets) = super::kdoc::reference_at(&an, unit, link).expect("a link");
    assert_eq!(&text[span], "User");
    assert_eq!(an.decl(targets[0].sym().unwrap()).name, "User");

    // `[User.name]` names the property.
    let member = text.find("[User.name]").unwrap() + 7;
    let (span, targets) = super::kdoc::reference_at(&an, unit, member).expect("a member link");
    assert_eq!(&text[span], "name");
    assert_eq!(an.decl(targets[0].sym().unwrap()).name, "name");
    let hover = features::reference_hover(&an, unit, member).expect("a hover");
    assert!(hover["contents"]["value"].as_str().unwrap().contains("val name: String"), "{hover}");

    // The links are references of the class: both name it, and find references lists them.
    let group = super::kdoc::group_at(&an, unit, link).expect("the class");
    let refs = features::doc_references(&an, &group, &features::Control::default()).expect("a search");
    assert!(refs.iter().any(|(u, r)| *u == unit && r.start == link - 1), "{refs:?}");
    assert!(refs.iter().all(|(_, r)| &text[r.clone()] == "User"), "{refs:?}");
    assert_eq!(refs.len(), 2, "{refs:?}");
}

#[test]
fn hovers_show_see_links_and_sample_code() {
    let src = "package app\n\nclass User\n\n/**\n * Makes a user.\n * @see User\n * @sample app.sample\n */\nfun make() = User()\n\nfun sample() {\n    val user = make()\n    println(user)\n}\n\nfun call() = ma$0ke()\n";
    let f = fixture(&[("src/app/Main.kt", src)]);
    let hover = f.hover();
    assert!(hover.contains("**See also** [`User`](") && hover.contains("#L3)"), "{hover}");
    assert!(hover.contains("**Sample** `app.sample`") && hover.contains("val user = make()\nprintln(user)"), "{hover}");
}

#[test]
fn generates_kdoc_for_a_declaration_without_one() {
    let src = "package app\n\nfun sp$0lit(text: String, limit: Int): List<String> {\n    if (limit < 1) throw IllegalArgumentException(\"limit\")\n    return listOf(text)\n}\n\nfun log(message: String) {}\n";
    let f = fixture(&[("src/app/Main.kt", src)]);
    assert_eq!(
        f.applied(Vec::new(), "Generate KDoc"),
        "package app\n\n/**\n * @param text\n * @param limit\n * @return\n * @throws IllegalArgumentException\n */\nfun split(text: String, limit: Int): List<String> {\n    if (limit < 1) throw IllegalArgumentException(\"limit\")\n    return listOf(text)\n}\n\nfun log(message: String) {}\n"
    );

    // A declaration with a comment, or with nothing to document, gets no action.
    let f = fixture(&[("src/app/Main.kt", "/** Logs. */\nfun l$0og(message: String) {}\n")]);
    assert!(!f.titles(Vec::new()).contains(&"Generate KDoc".to_owned()));
    let f = fixture(&[("src/app/Main.kt", "fun lo$0g() {}\n")]);
    assert!(!f.titles(Vec::new()).contains(&"Generate KDoc".to_owned()));
}

/// The names completed at the cursor of a single-file project.
fn completes(src: &str) -> Vec<String> {
    fixture(&[("src/Main.kt", src)]).labels()
}

#[test]
fn context_parameters_and_receivers_are_in_scope() {
    let logger = "class Logger { fun log(s: String) {} }\nclass Scope { fun launch() {} }\n";
    let labels = completes(&format!("{logger}context(logger: Logger) fun f() {{\n    logger.$0\n}}\n"));
    assert!(has(&labels, "log"), "a context parameter: {labels:?}");
    let labels = completes(&format!("{logger}context(logger: Logger, scope: Scope) fun f() {{\n    $0\n}}\n"));
    assert!(has(&labels, "logger") && has(&labels, "scope"), "{labels:?}");

    // A context receiver is an implicit receiver: its members are in scope, and `this@Scope` is it.
    let labels = completes(&format!("{logger}context(Scope) fun g() {{\n    lau$0\n}}\n"));
    assert!(has(&labels, "launch"), "{labels:?}");
    let labels = completes(&format!("{logger}context(Logger, Scope) fun g() {{\n    this@Scope.$0\n}}\n"));
    assert!(has(&labels, "launch") && !has(&labels, "log"), "{labels:?}");
    let labels = completes(&format!("{logger}context(Logger) fun Scope.g() {{\n    $0\n}}\n"));
    assert!(has(&labels, "launch") && has(&labels, "log"), "an extension receiver and a context receiver: {labels:?}");

    // Hover and go to definition on a use of a context parameter.
    let f = fixture(&[("src/Main.kt", &format!("{logger}context(logger: Logger) fun f() {{\n    logg$0er.log(\"x\")\n}}\n"))]);
    assert!(f.hover().contains("logger: Logger"), "{}", f.hover());
}

#[test]
fn lambdas_destructure_with_types_and_underscores() {
    let labels = completes("fun main() {\n    mapOf(\"a\" to 1).forEach { (_, value) -> value.$0 }\n}\n");
    assert!(has(&labels, "shl"), "{labels:?}");
    let labels = completes("fun main() {\n    mapOf(\"a\" to 1).forEach { (key, _) -> key.$0 }\n}\n");
    assert!(has(&labels, "uppercase"), "{labels:?}");
    // The written type gives the components their types.
    let decl = "fun call(block: (Any) -> Unit) {}\n";
    let labels = completes(&format!("{decl}fun main() {{\n    call {{ (a, b): Pair<String, Int> -> a.$0 }}\n}}\n"));
    assert!(has(&labels, "uppercase"), "{labels:?}");
    let labels = completes(&format!("{decl}fun main() {{\n    call {{ (a, b): Pair<String, Int> -> b.$0 }}\n}}\n"));
    assert!(has(&labels, "shl"), "{labels:?}");
    let labels = completes(&format!("{decl}fun main() {{\n    call {{ (_, b): Pair<String, Int> -> b.$0 }}\n}}\n"));
    assert!(has(&labels, "shl") && !has(&labels, "_"), "{labels:?}");
    // `_` for an unused parameter does not shift the others.
    let decl = "fun call(block: (Int, String) -> Unit) {}\n";
    let labels = completes(&format!("{decl}fun main() {{\n    call {{ _, s -> s.$0 }}\n}}\n"));
    assert!(has(&labels, "uppercase") && !has(&labels, "_"), "{labels:?}");
    // A destructured parameter counts as one when the overload is picked.
    let decl = "fun call(block: (Pair<String, Int>) -> Unit) {}\nfun call(block: (Int, Int) -> Unit) {}\n";
    let labels = completes(&format!("{decl}fun main() {{\n    call {{ (a, b) -> a.$0 }}\n}}\n"));
    assert!(has(&labels, "uppercase"), "{labels:?}");
    // Typed components of a `for` loop.
    let labels = completes("fun main() {\n    for ((k: String, v) in mapOf(\"a\" to 1)) {\n        k.$0\n    }\n}\n");
    assert!(has(&labels, "uppercase"), "{labels:?}");
}

#[test]
fn lambdas_in_defaults_supertypes_and_delegation_have_scopes() {
    let labels = completes("class A(val n: String, val f: () -> Int = { n.$0 })\n");
    assert!(has(&labels, "length"), "a primary constructor's default: {labels:?}");
    let labels = completes("fun g(a: String, b: () -> Int = { a.$0 }) {}\n");
    assert!(has(&labels, "length"), "a function's default: {labels:?}");
    let labels = completes("fun g(a: String, b: (String) -> Int = { it.$0 }) {}\n");
    assert!(has(&labels, "length"), "`it` in a default: {labels:?}");
    let labels = completes("open class B(val f: (String) -> Unit)\nclass A(val name: String) : B({ it.$0 })\n");
    assert!(has(&labels, "length"), "a supertype's constructor argument: {labels:?}");
    let labels = completes("open class B(val f: () -> Unit)\nclass A(val name: String) : B({ name.$0 })\n");
    assert!(has(&labels, "length"), "the constructor's parameters in a supertype argument: {labels:?}");
    let labels = completes("class A(val name: String) : Runnable by lazy { name.$0 }\n");
    assert!(has(&labels, "length"), "a delegate's lambda: {labels:?}");
    let labels = completes("class A(val name: String) : Runnable by lazy { name.length }\n{\n    val x = nam$0\n}\n");
    assert!(has(&labels, "name"), "the body after the delegate: {labels:?}");
    let labels = completes("enum class E(val f: () -> String) { P({ \"x\".$0 }) }\n");
    assert!(has(&labels, "length"), "an enum entry's argument: {labels:?}");
    // Go to definition from inside.
    let f = fixture(&[("src/Main.kt", "class A(val name: String, val f: () -> Int = { nam$0e.length })\n")]);
    assert_eq!(f.definition().map(|d| d.1), Some("class A(val name: String, val f: () -> Int = { name.length })".into()));
}

#[test]
fn types_properties_by_their_accessors_and_knows_field() {
    let labels = completes("class A {\n    val x get() { return \"s\" }\n}\nfun main() {\n    A().x.$0\n}\n");
    assert!(has(&labels, "length"), "a getter with a body: {labels:?}");
    let labels = completes(
        "class A {\n    val x get() {\n        if (true) return \"s\"\n        return \"t\"\n    }\n}\nfun main() {\n    A().x.$0\n}\n",
    );
    assert!(has(&labels, "length"), "{labels:?}");
    let labels = completes("class A {\n    val x get() = 1\n    val y get(): String { return \"\" }\n}\nfun main() {\n    A().y.$0\n}\n");
    assert!(has(&labels, "length"), "a getter with a declared type: {labels:?}");
    let labels = completes("var name: String = \"\"\n    get() = field.$0\n");
    assert!(has(&labels, "length"), "`field` has the property's type: {labels:?}");
    let labels =
        completes("class A {\n    var count = 1\n        set(value) {\n            field = value\n            value.$0\n        }\n}\n");
    assert!(has(&labels, "shl"), "`value` of a setter: {labels:?}");
    let labels = completes("class A {\n    var count = 1\n        set(value) {\n            fie$0\n        }\n}\n");
    assert!(has(&labels, "field"), "{labels:?}");
    let labels = completes("class A {\n    var count = 1\n        get() = fie$0\n}\n");
    assert!(has(&labels, "field"), "{labels:?}");
    // `field` is not a name outside accessors.
    let labels = completes("class A {\n    var count = 1\n    fun f() { fie$0 }\n}\n");
    assert!(!has(&labels, "field"), "{labels:?}");
    // A `return` in a lambda is not the getter's.
    let labels = completes(
        "class A {\n    val x get() {\n        listOf(1).forEach { return@forEach }\n        return \"s\"\n    }\n}\nfun main() {\n    A().x.$0\n}\n",
    );
    assert!(has(&labels, "length"), "{labels:?}");
}

#[test]
fn where_clauses_and_definitely_non_nullable_types_bound_type_parameters() {
    let labels = completes("fun <T> f(x: T): Int\n    where T : CharSequence {\n    x.$0\n    return 1\n}\n");
    assert!(has(&labels, "length"), "{labels:?}");
    let labels = completes("fun <T, U> f(x: T, y: U)\n    where\n        T : CharSequence,\n        U : Number {\n    y.$0\n}\n");
    assert!(has(&labels, "toInt"), "a clause over several lines: {labels:?}");
    let labels = completes("class Box<T>(val t: T)\n    where T : CharSequence {\n    fun f() = t.$0\n}\n");
    assert!(has(&labels, "length"), "a class's: {labels:?}");
    let labels = completes("fun <T : CharSequence?> f(x: T & Any) {\n    x.$0\n}\n");
    assert!(has(&labels, "length"), "{labels:?}");
    let labels = completes("fun <T : CharSequence> f(x: List<T & Any>) {\n    x.first().$0\n}\n");
    assert!(has(&labels, "length"), "{labels:?}");
}

#[test]
fn reads_annotated_and_suspend_function_types_for_lambdas() {
    let labels = completes("fun call(block: @Composable (String) -> Unit) {}\nfun main() {\n    call { it.$0 }\n}\n");
    assert!(has(&labels, "length"), "{labels:?}");
    let labels = completes("fun call(block: (@Composable (String) -> Unit)?) {}\nfun main() {\n    call { it.$0 }\n}\n");
    assert!(has(&labels, "length"), "{labels:?}");
    let labels = completes("fun call(block: suspend (String) -> Unit) {}\nfun main() {\n    call { it.$0 }\n}\n");
    assert!(has(&labels, "length"), "{labels:?}");
    let labels = completes("fun call(block: suspend String.() -> Unit) {}\nfun main() {\n    call { len$0 }\n}\n");
    assert!(has(&labels, "length"), "{labels:?}");
}

#[test]
fn secondary_constructors_and_init_blocks_see_the_parameters() {
    let labels = completes("class A(val a: String) {\n    constructor(x: String, y: Int) : this(x.$0)\n}\n");
    assert!(has(&labels, "length"), "{labels:?}");
    let labels = completes("class A(val a: String) {\n    constructor(x: String) : this(x.length.toString()) {\n        x.$0\n    }\n}\n");
    assert!(has(&labels, "length"), "{labels:?}");
    let labels = completes("class A(val name: String, count: Int) {\n    init { name.$0 }\n    init { println(count) }\n}\n");
    assert!(has(&labels, "length"), "{labels:?}");
    let labels = completes("class A(val name: String, count: Int) {\n    init { println(name) }\n    init { count.$0 }\n}\n");
    assert!(has(&labels, "shl"), "a second init block: {labels:?}");
    let labels = completes("open class P(val s: String)\nclass A : P {\n    constructor(x: String) : super(x.$0)\n}\n");
    assert!(has(&labels, "length"), "{labels:?}");
    let f = fixture(&[("src/Main.kt", "class A(val name: String, count: Int) {\n    init { println(cou$0nt) }\n}\n")]);
    assert_eq!(f.definition().map(|d| d.1), Some("class A(val name: String, count: Int) {".into()));
}

#[test]
fn scripts_have_args_and_top_level_statements() {
    let f = fixture(&[("src/run.kts", "val n = 1\nargs.$0\n")]);
    assert!(has(&f.labels(), "size"), "{:?}", f.labels());
    let f = fixture(&[("src/run.kts", "fun helper() = \"s\"\nval n = helper()\nlistOf(n).forEach { it.$0 }\n")]);
    assert!(has(&f.labels(), "length"), "{:?}", f.labels());
    let f = fixture(&[("src/run.kts", "@file:DependsOn(\"g:a:1\")\nval n = 1\nn.$0\n")]);
    assert!(has(&f.labels(), "shl"), "{:?}", f.labels());
    // A Kotlin file has no `args`.
    let f = fixture(&[("src/Main.kt", "val n = ar$0\n")]);
    assert!(!has(&f.labels(), "args"), "{:?}", f.labels());
    // The class of the facade of a file.
    let f = fixture(&[("src/Util.kt", "@file:JvmName(\"Helpers\")\n@file:JvmMultifileClass\npackage app\n\nfun f() = 1\n")]);
    let file = &f.proj.units[0].file;
    assert_eq!(file.jvm_name.as_deref(), Some("Helpers"));
    assert!(file.jvm_multifile);
}

const STREAM_JDK: &[(&str, &str)] = &[
    (
        "java.base/module-info.java",
        "module java.base {\n    exports java.lang;\n    exports java.util;\n    exports java.util.function;\n    exports java.util.stream;\n}\n",
    ),
    (
        "java.base/java/lang/String.java",
        "package java.lang;\n\npublic final class String implements CharSequence {\n    public int length() {\n        return 0;\n    }\n}\n",
    ),
    ("java.base/java/lang/CharSequence.java", "package java.lang;\n\npublic interface CharSequence {\n    int length();\n}\n"),
    (
        "java.base/java/util/function/Predicate.java",
        "package java.util.function;\n\npublic interface Predicate<T> {\n    boolean test(T t);\n\n    default Predicate<T> negate() {\n        return null;\n    }\n\n    static <T> Predicate<T> not(Predicate<? super T> target) {\n        return null;\n    }\n}\n",
    ),
    (
        "java.base/java/util/function/Consumer.java",
        "package java.util.function;\n\npublic interface Consumer<T> {\n    void accept(T t);\n}\n",
    ),
    (
        "java.base/java/util/stream/Stream.java",
        "package java.util.stream;\n\nimport java.util.function.Consumer;\nimport java.util.function.Predicate;\n\npublic interface Stream<T> {\n    Stream<T> filter(Predicate<? super T> predicate);\n\n    void forEach(Consumer<? super T> action);\n}\n",
    ),
    ("java.base/java/util/List.java", "package java.util;\n\npublic interface List<E> {\n    java.util.stream.Stream<E> stream();\n}\n"),
];

#[test]
fn lambda_of_a_java_functional_interface_gets_its_parameter_types() {
    let src = "import java.util.List\n\nfun f(list: List<String>) {\n    list.stream().filter {\n        it.$0\n    }\n}\n";
    let f = fixture_with_jdk(&[("src/Main.kt", src)], STREAM_JDK);
    assert!(f.labels().contains(&"length".to_owned()), "{:?}", f.labels());
    let src = "import java.util.List\n\nfun f(list: List<String>) {\n    list.stream().forEach { s ->\n        s.$0\n    }\n}\n";
    let f = fixture_with_jdk(&[("src/Main.kt", src)], STREAM_JDK);
    assert!(f.labels().contains(&"length".to_owned()), "{:?}", f.labels());
}

/// `it` in the lambda of `stream().filter { }` against the JDK on this machine (`JAVA_HOME`).
#[test]
#[ignore]
fn completes_java_lambda_parameters_in_the_real_jdk() {
    let home = PathBuf::from(std::env::var("JAVA_HOME").expect("JAVA_HOME"));
    let dir = tempfile::tempdir().unwrap();
    let jdk = super::jdk::Jdk::open(&home, &dir.path().join("cache"), "JDK").expect("a JDK with sources");
    let src = "fun main() {\n    val list = mutableListOf<String>(\"Hello\", \"Olneta jai\")\n    list.stream().filter {\n        it.$0 >= 5\n    }.forEach(System.out::println)\n}\n";
    let f = fixture_on(&[("src/Main.kt", src)], Some(Arc::new(jdk)), Some(dir), Vec::new());
    let labels = f.labels();
    assert!(has(&labels, "length") && has(&labels, "uppercase"), "{labels:?}");
}

/// The inlay hints of `src` as `(line, column, label)`.
fn inlays(src: &str, options: super::inlay::Options) -> Vec<(u64, u64, String)> {
    let f = fixture(&[("src/Main.kt", src)]);
    let u = UnitId { lib: false, unit: 0 };
    super::inlay::hints(&f.an(), u, None, options)
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (h["position"]["line"].as_u64().unwrap(), h["position"]["character"].as_u64().unwrap(), h["label"].as_str().unwrap().to_owned())
        })
        .collect()
}

#[test]
fn shows_inferred_types_but_not_obvious_ones() {
    let src = "class Box\nfun names(): List<String> = emptyList()\nfun main() {\n    val a = names()\n    val b = 1\n    val c = Box()\n    val d: Int = 2\n    val e = \"x\"\n}\n";
    let got = inlays(src, Default::default());
    assert_eq!(got, vec![(3, 9, ": List<String>".to_owned())], "{got:?}");
}

#[test]
fn shows_lambda_parameters_it_and_receivers() {
    let src =
        "fun main() {\n    val xs = listOf(\"a\")\n    xs.let { it.length }\n    buildString { append(\"x\") }\n    xs.let { s -> s }\n}\n";
    let got = inlays(src, Default::default());
    assert!(got.contains(&(2, 12, "it: List<String>".to_owned())), "{got:?}");
    assert!(got.contains(&(3, 17, "this: StringBuilder".to_owned())), "{got:?}");
    assert!(got.contains(&(4, 14, ": List<String>".to_owned())), "{got:?}");
}

#[test]
fn shows_parameter_names_of_literal_arguments() {
    let src = "fun greet(name: String, loud: Boolean, times: Int = 1) {}\nfun main() {\n    val n = \"a\"\n    greet(\"a\", true)\n    greet(n, false, times = 2)\n    greet(\"b\", loud = true)\n}\n";
    let got = inlays(src, Default::default());
    let names: Vec<_> = got.iter().map(|h| (h.0, h.1, h.2.as_str())).collect();
    assert_eq!(names, vec![(3, 10, "name:"), (3, 15, "loud:"), (4, 13, "loud:"), (5, 10, "name:")], "{got:?}");
}

#[test]
fn inlay_hints_can_be_switched_off() {
    let src = "fun f(a: Int) {}\nfun g(): List<String> = emptyList()\nfun main() {\n    val x = g()\n    f(1)\n    g().let { it }\n}\n";
    let off = |o: &str| super::inlay::Options::from_json(&serde_json::from_str(o).unwrap());
    assert_eq!(inlays(src, Default::default()).len(), 3);
    assert_eq!(inlays(src, off(r#"{"inlayHints":{"types":false}}"#)).len(), 2);
    assert_eq!(inlays(src, off(r#"{"inlayHints.parameterNames":false}"#)).len(), 2);
    assert_eq!(inlays(src, off(r#"{"inlayHints":{"implicitIt":false}}"#)).len(), 2);
}

// ----- operator conventions -----

const OPERATORS: &str = "class Money(val cents: Int) {
    operator fun plus(o: Money): Money = this
    operator fun plus(n: Int): Receipt = Receipt()
    operator fun minus(o: Money): Money = this
    operator fun unaryMinus(): Debt = Debt()
    operator fun not(): Flag = Flag()
    operator fun inc(): Money = this
    operator fun dec(): Money = this
    operator fun compareTo(o: Money): Int = 0
    operator fun contains(c: Coin): Boolean = true
    operator fun get(i: Int): Coin = Coin()
    operator fun get(name: String): Receipt = Receipt()
    operator fun set(i: Int, c: Coin) {}
    operator fun invoke(n: Int): Receipt = Receipt()
    operator fun plusAssign(c: Coin) {}
    operator fun iterator(): CoinIterator = CoinIterator()
    operator fun component1(): Coin = Coin()
    operator fun component2(): Receipt = Receipt()
}
class Coin { val metal = 1 }
class Receipt { val lines = 1 }
class Debt { val owed = 1 }
class Flag { val raised = 1 }
class CoinIterator {
    operator fun hasNext(): Boolean = false
    operator fun next(): Coin = Coin()
}
operator fun Money.times(k: Int): Receipt = Receipt()
operator fun Money.div(k: Int): Debt = Debt()
";

fn with_operators(code: &str) -> Fixture {
    fixture(&[("src/Main.kt", &format!("{OPERATORS}fun f(a: Money, b: Money, c: Coin) {{\n{code}\n}}\n"))])
}

#[test]
fn infers_the_result_of_every_operator_function() {
    let sees = |code: &str, name: &str| offers(&format!("{OPERATORS}fun f(a: Money, b: Money, c: Coin) {{\n{code}\n}}\n"), name);
    // The overload follows the right operand.
    assert!(sees("(a + b).$0", "cents"));
    assert!(sees("(a + 1).$0", "lines"));
    assert!(sees("val t = a - b\nt.$0", "cents"));
    // Extensions, unary, prefix and postfix operators.
    assert!(sees("(a * 2).$0", "lines"));
    assert!(sees("(a / 2).$0", "owed"));
    assert!(sees("(-a).$0", "owed"));
    assert!(sees("(!a).$0", "raised"));
    assert!(sees("var m = a\nval t = m++\nt.$0", "cents"));
    assert!(sees("var m = a\nval t = ++m\nt.$0", "cents"));
    // Indexing picks `get` by the index's type; invoke by call.
    assert!(sees("a[0].$0", "metal"));
    assert!(sees("a[\"k\"].$0", "lines"));
    assert!(sees("a(3).$0", "lines"));
    // Containment and comparison are Boolean.
    assert!(sees("(c in a).$0", "not"));
    assert!(sees("(a < b).$0", "not"));
    assert!(sees("(a == b).$0", "not"));
    // `for` takes the element type from `next`, destructuring from `componentN`.
    assert!(sees("for (x in a) {\n x.$0\n}", "metal"));
    assert!(sees("val (p, q) = a\nq.$0", "lines"));
    assert!(sees("val (p, q) = a\np.$0", "metal"));
    // The standard library's.
    assert!(sees("val t = \"a\" + 1\nt.$0", "length"));
    assert!(sees("val xs = listOf(\"a\")\nxs[0].$0", "length"));
    assert!(sees("for (e in 1..10) {\n e.$0\n}", "shl"));
}

#[test]
fn infers_operators_through_properties_and_delegates() {
    let code = "class Counter { operator fun invoke(n: Int): Receipt = Receipt() }
class Cell { operator fun getValue(r: Any?, p: kotlin.reflect.KProperty<*>): Coin = Coin() }
class Receipt { val lines = 1 }
class Coin { val metal = 1 }
class Holder(val count: Counter) {
    val cell by Cell()
    fun f() {
        count(1).$0
    }
}
";
    assert!(offers(code, "lines"));
    assert!(offers(&code.replace("count(1).$0", "cell.$0"), "metal"));
}

#[test]
fn goes_to_the_operator_function_of_an_operator() {
    let go = |code: &str| with_operators(code).definition().map(|d| d.1);
    // The overload that the right operand fits.
    assert_eq!(go("a $0+ b"), Some("operator fun plus(o: Money): Money = this".into()));
    assert_eq!(go("a $0+ 1"), Some("operator fun plus(n: Int): Receipt = Receipt()".into()));
    assert_eq!(go("a $0- b"), Some("operator fun minus(o: Money): Money = this".into()));
    // A longer expression ends the operand at the operator of lower precedence.
    assert_eq!(go("val t = a + b $0* 2"), Some("operator fun Money.times(k: Int): Receipt = Receipt()".into()));
    assert_eq!(go("val t = a $0+ b * 2"), Some("operator fun plus(o: Money): Money = this".into()));
    assert_eq!(go("val t = $0-a"), Some("operator fun unaryMinus(): Debt = Debt()".into()));
    assert_eq!(go("val t = $0!a"), Some("operator fun not(): Flag = Flag()".into()));
    assert_eq!(go("var m = a\n m+$0+"), Some("operator fun inc(): Money = this".into()));
    assert_eq!(go("a $0< b"), Some("operator fun compareTo(o: Money): Int = 0".into()));
    assert_eq!(go("c $0in a"), Some("operator fun contains(c: Coin): Boolean = true".into()));
    assert_eq!(go("a[\"k\"$0]"), Some("operator fun get(name: String): Receipt = Receipt()".into()));
    assert_eq!(go("a[0$0]"), Some("operator fun get(i: Int): Coin = Coin()".into()));
    assert_eq!(go("a[0$0] = c"), Some("operator fun set(i: Int, c: Coin) {}".into()));
    assert_eq!(go("var m = a\n m $0+= c"), Some("operator fun plusAssign(c: Coin) {}".into()));
    assert_eq!(go("for (x $0in a) {}"), Some("operator fun iterator(): CoinIterator = CoinIterator()".into()));
    // `+=` without `plusAssign` is `a = a + b`.
    assert_eq!(go("var m = a\n m $0+= b"), Some("operator fun plus(o: Money): Money = this".into()));
    assert_eq!(go("val v = a $0(2)"), Some("operator fun invoke(n: Int): Receipt = Receipt()".into()));
    // A call of a function is no invoke.
    assert_eq!(
        fixture(&[(
            "src/Main.kt",
            "fun g(n: Int) = n
fun f() {
    g $0(2)
}
"
        )])
        .definition()
        .map(|d| d.1),
        None
    );
}

#[test]
fn goes_to_the_operator_functions_of_the_standard_library() {
    let go = |code: &str| fixture(&[("src/Main.kt", code)]).definition();
    assert!(go("fun f() {\n    val s = \"a\" $0+ 1\n}\n").is_some());
    assert!(go("fun f(l: List<String>) {\n    val s = l $0[0]\n}\n").is_some_and(|d| d.1.contains("operator fun get(index: Int): E")));
    assert!(go("fun f(m: MutableList<String>) {\n    m $0[0] = \"a\"\n}\n").is_some_and(|d| d.1.contains("operator fun set(index: Int")));
    assert!(go("fun f() {\n    for (e $0in 1..3) {}\n}\n").is_some());
    assert!(go("fun f(x: Int) {\n    val b = x $0in 1..3\n}\n").is_some());
}

#[test]
fn hovers_an_operator_with_its_function() {
    let f = with_operators("val t = a $0+ 1");
    assert!(f.hover().contains("fun plus(n: Int): Receipt"), "{}", f.hover());
}

/// The source text of the places that call the operator function `name` as syntax.
fn operator_uses(code: &str, name: &str) -> Vec<String> {
    let f = with_operators(code);
    let an = f.an();
    let u = UnitId { lib: false, unit: 0 };
    let (file, text) = (an.file(u), &an.unit(u).text);
    let tok = (0..file.lexed.tokens.len())
        .find(|&i| file.lexed.text(text, i) == name && !file.decls_named_at(&file.lexed.tokens[i].span()).is_empty())
        .expect("the declaration");
    let sym = features::targets_at(&an, u, tok).into_iter().find_map(|t| t.sym()).unwrap();
    let group = features::rename_group(&an, sym);
    let found = super::operators::operator_uses(&an, &group, &features::Control::default()).unwrap();
    found.iter().map(|(_, r)| text[r.clone()].to_owned()).collect()
}

#[test]
fn finds_the_uses_of_an_operator_function() {
    let code = "val x = a + b\nval y = a + 2\nvar m = a\nm += b\nm += c\nm++";
    // `plus(Money)`: the first `+`, and `+=` with a Money (no `plusAssign` takes one).
    assert_eq!(operator_uses(code, "plus"), vec!["+".to_owned(), "+=".to_owned()]);
    assert_eq!(operator_uses(code, "plusAssign"), vec!["+=".to_owned()]);
    assert_eq!(operator_uses(code, "inc"), vec!["++".to_owned()]);
    assert_eq!(operator_uses("val p = a[0]\na[1] = c\nval q = a[\"k\"]", "get").len(), 1);
    assert_eq!(operator_uses("val p = a[0]\na[1] = c", "set"), vec!["[".to_owned()]);
    assert_eq!(operator_uses("for (x in a) {}\nval r = c in a", "iterator"), vec!["in".to_owned()]);
    assert_eq!(operator_uses("for (x in a) {}\nval r = c in a", "contains"), vec!["in".to_owned()]);
    assert_eq!(operator_uses("val (p, q) = a", "component1"), vec!["p".to_owned()]);
    assert_eq!(operator_uses("val r = a(1)\nval s = b(2)", "invoke").len(), 2);
}

#[test]
fn refuses_to_rename_an_operator_function_that_syntax_calls() {
    let f = with_operators("val t = a + b");
    let an = f.an();
    let u = UnitId { lib: false, unit: 0 };
    let text = &an.unit(u).text;
    let at = text.find("fun plus(o").unwrap() + "fun p".len();
    let found = super::rename::subject(&an, u, at).expect("a subject");
    let error =
        super::rename::rename(&an, &super::rename::Env::default(), &found, "add", &features::Control::default()).expect_err("a refusal");
    assert!(error.contains("operator function") && error.contains("`+`"), "{error}");
}

#[test]
fn completes_operator_names_after_operator_fun() {
    let f = fixture(&[("src/Main.kt", "class V {\n    operator fun pl$0\n}\n")]);
    let items = f.items();
    let plus = items.iter().find(|i| i["label"] == "plus").expect("plus");
    assert_eq!(plus["detail"], "operator fun plus(other: T): T");
    assert!(plus["textEdit"]["newText"].as_str().unwrap().starts_with("plus(${1:other}: ${2:T}): T"), "{plus}");
    assert!(items.iter().any(|i| i["label"] == "plusAssign"));
    // Not after plain `fun`.
    let f = fixture(&[("src/Main.kt", "class V {\n    fun pl$0\n}\n")]);
    assert!(!has(&f.labels(), "plusAssign"));
}

/// The operator problems of the file `code`.
fn operator_problems(code: &str) -> Vec<String> {
    let f = fixture(&[("src/Main.kt", code)]);
    let an = f.an();
    super::operators::problems(&an, UnitId { lib: false, unit: 0 }).into_iter().map(|p| p.message).collect()
}

#[test]
fn reports_operators_that_cannot_work() {
    let decls = "class Money(val c: Int) {\n    fun plus(o: Money): Money = this\n    operator fun minus(o: Money): Money = this\n}\n";
    let body = |code: &str| format!("{decls}fun f(a: Money, b: Money) {{\n{code}\n}}\n");
    let problems = operator_problems(&body("val x = a + b"));
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("`operator` modifier is required on `plus`"), "{problems:?}");
    assert!(operator_problems(&body("val x = a - b")).is_empty());
    let problems = operator_problems(&body("val x = a * b"));
    assert!(problems.iter().any(|m| m.contains("no operator function `times`")), "{problems:?}");
    let problems = operator_problems(&body("val x = a - 1"));
    assert!(problems.iter().any(|m| m.contains("`minus` that takes `Int`")), "{problems:?}");
    let problems = operator_problems(&body("val x = a[0]"));
    assert!(problems.iter().any(|m| m.contains("no operator function `get`")), "{problems:?}");
    // Standard types and classes with supertypes the server does not know stay quiet.
    assert!(operator_problems("fun f() {\n    val x = 1 + 2L\n    val s = \"a\" + 1\n}\n").is_empty());
    assert!(operator_problems("class A : Unknown()\nfun f(a: A) {\n    val x = a + a\n}\n").is_empty());
    // A comparison needs `compareTo`, which `Comparable` brings.
    assert!(
        operator_problems(
            "class V : Comparable<V> {\n    override fun compareTo(other: V): Int = 0\n}\nfun f(a: V, b: V) {\n    val x = a < b\n}\n"
        )
        .is_empty()
    );
}

#[test]
fn type_arguments_are_no_comparisons() {
    // The `>` closing type arguments is no `compareTo` call (issue #374).
    let page = "data class Page<T>(val content: List<T> = emptyList(), val page: Int = 0)\n";
    assert!(operator_problems(page).is_empty(), "{:?}", operator_problems(page));
    let call = format!("{page}fun <T> local(block: () -> T): T = block()\nfun f() {{\n    val a = local<Page<String>> {{ Page() }}\n}}\n");
    assert!(operator_problems(&call).is_empty(), "{:?}", operator_problems(&call));
    let call = format!("{page}fun f() {{\n    val a = Page<String>()\n    val b = Page<List<String>>(listOf())\n}}\n");
    assert!(operator_problems(&call).is_empty(), "{:?}", operator_problems(&call));
    // Type arguments in annotations, parameters, return types and nested arguments.
    let types = format!(
        "{page}fun g(p: Page<String>, q: Map<String, Page<Int>>?): Page<List<String>> = Page()
val x: Page<String> = Page()
val y: List<Page<String>> = listOf()
class H(val p: Page<String>, val q: Page<Int>)
"
    );
    assert!(operator_problems(&types).is_empty(), "{:?}", operator_problems(&types));
    // A real comparison of a class without `compareTo` is still reported.
    let compare = "class Money(val c: Int)\nfun f(a: Money, b: Money) {\n    val x = a > b\n}\n";
    assert!(operator_problems(compare).iter().any(|m| m.contains("no operator function `compareTo`")));
}

#[test]
fn prefers_the_operator_when_a_name_ends_at_it() {
    let f = with_operators("val x = a$0[0]");
    let (u, offset) = f.cursor();
    let an = f.an();
    let tok = features::token_at(&an, u, offset).unwrap();
    assert_eq!(an.file(u).lexed.kind(tok), Some(super::lexer::Tok::LBracket));
    // A name followed by the parenthesis of a call stays the name.
    let f = fixture(&[("src/Main.kt", "fun g(n: Int) = n\nfun f() {\n    g$0(2)\n}\n")]);
    let (u, offset) = f.cursor();
    let an = f.an();
    let tok = features::token_at(&an, u, offset).unwrap();
    assert_eq!(an.file(u).lexed.kind(tok), Some(super::lexer::Tok::Ident));
}

#[test]
fn shows_the_operator_functions_in_inlay_hints() {
    let f = with_operators("val x = a + b\nval y = a + 1\nval z = 1 + 2");
    let an = f.an();
    let on = super::inlay::Options { operators: true, ..super::inlay::Options::default() };
    let labels = |options| {
        super::inlay::hints(&an, UnitId { lib: false, unit: 0 }, None, options)
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["label"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let shown = labels(on);
    assert!(shown.iter().any(|l| l == "plus(o: Money)"), "{shown:?}");
    assert!(shown.iter().any(|l| l == "plus(n: Int)"), "{shown:?}");
    assert_eq!(shown.iter().filter(|l| l.starts_with("plus")).count(), 2, "{shown:?}");
    // Off unless asked for.
    assert!(!labels(super::inlay::Options::default()).iter().any(|l| l.starts_with("plus")));
}

#[test]
fn completes_operator_names_for_extensions_and_every_convention() {
    let f = fixture(&[("src/Main.kt", "class V\noperator fun V.pl$0\n")]);
    assert!(has(&f.labels(), "plus"));
    let f = fixture(&[("src/Main.kt", "class V\noperator fun <T> List<T>.$0\n")]);
    let labels = f.labels();
    for name in ["plus", "get", "set", "invoke", "compareTo", "iterator", "component3", "provideDelegate", "rangeUntil", "unaryMinus"] {
        assert!(has(&labels, name), "{name} in {labels:?}");
    }
    // Plain extensions get no operator names.
    let f = fixture(&[("src/Main.kt", "class V\nfun V.$0\n")]);
    assert!(!has(&f.labels(), "plusAssign"));
}

#[test]
fn mod_stands_in_for_rem() {
    let f = fixture(&[("src/Main.kt", "class N {\n    operator fun mod(k: Int): String = \"\"\n}\nfun f(n: N) {\n    (n $0% 2).\n}\n")]);
    assert!(f.definition().is_some_and(|d| d.1.contains("operator fun mod")));
}

const DELEGATES: &str = "import kotlin.reflect.KProperty
class Box<T>(val v: T) { operator fun getValue(r: Any?, p: KProperty<*>): T = v }
class Cell { operator fun getValue(r: Any?, p: KProperty<*>): String = \"\" }
class Provider { operator fun provideDelegate(r: Any?, p: KProperty<*>): Cell = Cell() }
class State<T>(val value: T)
operator fun <T> State<T>.getValue(r: Any?, p: KProperty<*>): T = value
fun <T> remember(f: () -> T): T = f()
fun <T> mutableStateOf(v: T): State<T> = State(v)
fun compute(): String = \"\"
fun <T> viewModels(): Lazy<T> = TODO()
fun <T> inject(): Lazy<T> = TODO()
class Foo { val tag = 1 }
";

#[test]
fn infers_every_kind_of_delegate() {
    let sees = |member: &str, name: &str| {
        offers(
            &format!(
                "{DELEGATES}class H {{
    {member}
}}
"
            ),
            name,
        )
    };
    let cases = [
        // The standard library's delegates.
        (
            "val v by lazy(LazyThreadSafetyMode.NONE) { \"x\" }
    fun f() { v.$0 }",
            "length",
        ),
        (
            "val v by lazy(::compute)
    fun f() { v.$0 }",
            "length",
        ),
        (
            "val v by lazy { compute() }
    fun f() { v.$0 }",
            "length",
        ),
        (
            "val m = mapOf<String, Int>()
    val v by m
    fun f() { v.$0 }",
            "coerceAtLeast",
        ),
        (
            "val m = mutableMapOf<String, Int>()
    var v by m
    fun f() { v.$0 }",
            "coerceAtLeast",
        ),
        // A generic class: the type argument is the property's type.
        (
            "val v by Box(\"s\")
    fun f() { v.$0 }",
            "length",
        ),
        // `provideDelegate` first, then `getValue` of what it returns.
        (
            "val v by Provider()
    fun f() { v.$0 }",
            "length",
        ),
        // An extension `getValue`, as Compose's `State` has.
        (
            "val v by remember { mutableStateOf(\"s\") }
    fun f() { v.$0 }",
            "length",
        ),
        (
            "val v by mutableStateOf(\"s\")
    fun f() { v.$0 }",
            "length",
        ),
        // The type from an explicit type argument, or the declared type.
        (
            "val v by inject<Foo>()
    fun f() { v.$0 }",
            "tag",
        ),
        (
            "val v: Foo by viewModels()
    fun f() { v.$0 }",
            "tag",
        ),
        (
            "val v: String by Cell()
    fun f() { v.$0 }",
            "length",
        ),
        // Another property.
        (
            "val b = \"s\"
    val v by ::b
    fun f() { v.$0 }",
            "length",
        ),
        (
            "val b = \"s\"
    val v by this::b
    fun f() { v.$0 }",
            "length",
        ),
    ];
    for (member, name) in cases {
        assert!(sees(member, name), "{member}");
    }
    // Local and top-level properties.
    assert!(offers(
        &format!(
            "{DELEGATES}fun f() {{
    val v by Cell()
    v.$0
}}
"
        ),
        "length"
    ));
    assert!(offers(
        &format!(
            "{DELEGATES}fun f() {{
    val v by lazy {{ \"x\" }}
    v.$0
}}
"
        ),
        "length"
    ));
    assert!(offers(
        &format!(
            "{DELEGATES}val top by Provider()
fun f() {{
    top.$0
}}
"
        ),
        "length"
    ));
}

#[test]
fn navigates_and_shows_delegated_properties() {
    let at = |code: &str| fixture(&[("src/Main.kt", &format!("{DELEGATES}{code}"))]);
    let get = "class Cell { operator fun getValue(r: Any?, p: KProperty<*>): String = \"\" }";
    let provide = "class Provider { operator fun provideDelegate(r: Any?, p: KProperty<*>): Cell = Cell() }";
    assert!(at("class H { val v$0 by Cell() }").hover().contains("val v: String"));
    assert!(at("class H { val v b$0y Cell() }").hover().contains("operator fun getValue"));
    assert_eq!(at("class H { val v b$0y Cell() }").definition().map(|d| d.1), Some(get.into()));
    assert_eq!(at("class H { val v b$0y Provider() }").definition().map(|d| d.1), Some(provide.into()));
    assert_eq!(at("class H { val v: String b$0y Cell() }").definition().map(|d| d.1), Some(get.into()));
    // The inferred type is an inlay hint, a declared one is not.
    let hints = inlays(
        &format!(
            "{DELEGATES}class H {{ val v by Cell()
 val w: String by Cell() }}
"
        ),
        Default::default(),
    );
    assert_eq!(hints.iter().map(|h| h.2.as_str()).collect::<Vec<_>>(), vec![": String"], "{hints:?}");
}

#[test]
fn offers_the_members_of_delegated_interfaces() {
    let decls = "interface Iface { fun run(): String
 val size: Int }
interface Other { fun other() }
class B : Iface, Other {
 override fun run() = \"\"
 override val size = 1
 override fun other() {} }
";
    let sees = |code: &str, name: &str| {
        offers(
            &format!(
                "{decls}{code}
"
            ),
            name,
        )
    };
    assert!(sees(
        "class A(b: B) : Iface by b {
 fun f() { this.$0 }
}",
        "run"
    ));
    assert!(sees(
        "class A(val b: B) : Iface by b {
 fun f() { this.$0 }
}",
        "run"
    ));
    assert!(sees(
        "class A : Iface by B() {
 fun f() { run().$0 }
}",
        "length"
    ));
    assert!(sees(
        "class A(b: B) : Iface by b
fun f(a: A) { a.$0 }",
        "size"
    ));
    assert!(sees(
        "class A(b: B, c: B) : Iface by b, Other by c
fun f(a: A) { a.$0 }",
        "other"
    ));
    let f = fixture(&[(
        "src/Main.kt",
        &format!(
            "{decls}class A(b: B) : Iface by b
fun f(a: A) {{ a.ru$0n() }}
"
        ),
    )]);
    assert_eq!(f.definition().map(|d| d.1), Some("interface Iface { fun run(): String".into()));
    // What is delegated is implemented: nothing to implement, and `override` offers it as delegated.
    let f = fixture(&[(
        "src/Main.kt",
        &format!(
            "{decls}class A(val b: B) : Iface by b$0
"
        ),
    )]);
    assert!(f.titles_but_kdoc(Vec::new()).is_empty(), "{:?}", f.titles(Vec::new()));
    let items = fixture(&[(
        "src/Main.kt",
        &format!(
            "{decls}class A(val b: B) : Iface by b {{
    override $0
}}
"
        ),
    )])
    .items();
    let run = items
        .iter()
        .find(|i| i["label"] == "run" && i["labelDetails"]["description"].as_str().is_some_and(|d| d.contains("delegation")))
        .unwrap_or_else(|| {
            panic!("run in {:?}", items.iter().map(|i| (i["label"].as_str(), i["_expected"].clone())).take(8).collect::<Vec<_>>())
        });
    assert_eq!(run["labelDetails"]["description"], "implemented by delegation to b");
    assert!(run["textEdit"]["newText"].as_str().unwrap().contains("return b.run()"), "{run}");
}

#[test]
fn reports_delegates_that_cannot_work() {
    let decls = "import kotlin.reflect.KProperty
class Cell { operator fun getValue(r: Any?, p: KProperty<*>): String = \"\" }
class Plain { fun getValue(r: Any?, p: KProperty<*>): String = \"\" }
class Empty
class Both { operator fun getValue(r: Any?, p: KProperty<*>): Int = 1
 operator fun setValue(r: Any?, p: KProperty<*>, v: Int) {} }
class Provider { operator fun provideDelegate(r: Any?, p: KProperty<*>): Cell = Cell() }
";
    let problems = |member: &str| {
        operator_problems(&format!(
            "{decls}class H {{
    {member}
}}
"
        ))
    };
    assert!(problems("val a by Cell()").is_empty());
    assert!(problems("val a: String by Cell()").is_empty());
    assert!(problems("val a: CharSequence by Cell()").is_empty());
    assert!(problems("val a by Provider()").is_empty());
    assert!(problems("var a by Both()").is_empty());
    assert!(problems("val a by lazy { 1 }").is_empty());
    assert!(problems("val a by mapOf(\"a\" to 1)").is_empty());
    let one = |member: &str, text: &str| {
        let found = problems(member);
        assert!(found.len() == 1 && found[0].contains(text), "{member}: {found:?}");
    };
    one("val a by Empty()", "has no `getValue`");
    one("val a by Plain()", "`operator` modifier is required on `getValue`");
    one("var a by Cell()", "has no `setValue`");
    one("val a: Int by Cell()", "not the declared `Int`");
    one("val a: Int by Provider()", "not the declared `Int`");
}

#[test]
fn infers_delegate_type_arguments_from_the_declared_type() {
    // Issue #514: `by viewModels()` leaves `VM` unbound, so the server falls back
    // to its upper bound. The compiler solves it from the declared property type.
    let decls = "import kotlin.reflect.KProperty
open class ViewModel
class MainViewModel : ViewModel()
class LazyVM<T>(val value: T) {
    operator fun getValue(thisRef: Any?, prop: KProperty<*>): T = value
}
fun <VM : ViewModel> viewModels(): LazyVM<VM> = TODO()
";
    let problems = |member: &str| {
        operator_problems(&format!(
            "{decls}class H {{
    {member}
}}
"
        ))
    };
    assert!(problems("val viewModel: MainViewModel by viewModels()").is_empty());
    // An explicit type argument already worked.
    assert!(problems("val viewModel: MainViewModel by viewModels<MainViewModel>()").is_empty());
    // A declared type the bound does not allow is still an error.
    let bad = problems("val s: String by viewModels()");
    assert!(bad.len() == 1 && bad[0].contains("not the declared"), "{bad:?}");
    // A bound type argument is not solved from the declared type.
    let bound = operator_problems(&format!(
        "{decls}fun <T> lazyBox(v: T): LazyVM<T> = TODO()
class H {{
    val viewModel: MainViewModel by lazyBox(ViewModel())
}}
"
    ));
    assert!(bound.len() == 1 && bound[0].contains("not the declared"), "{bound:?}");
}

// ----- completion outside expressions -----

impl Fixture {
    /// The items completed at the cursor with `opts`.
    fn items_with(&self, opts: CompletionOptions) -> Vec<Value> {
        let (u, offset) = self.cursor();
        features::completion(&self.an(), u, offset, opts, 0)["items"].as_array().unwrap().clone()
    }
}

#[test]
fn completes_the_package_of_the_directory() {
    let opts = || CompletionOptions { snippets: true, package: Some("app.model".into()) };
    let f = fixture(&[("src/app/model/User.kt", "package $0\n\nclass User\n")]);
    let items = f.items_with(opts());
    assert_eq!(item(&items, "app.model")["textEdit"]["newText"], "app.model");
    // The segments typed so far are replaced with the whole name.
    let f = fixture(&[("src/app/model/User.kt", "package app.mo$0\n\nclass User\n")]);
    let items = f.items_with(opts());
    assert_eq!(item(&items, "app.model")["textEdit"]["range"]["start"]["character"], 8, "{items:?}");
    // Something else typed gets nothing, and so does a file without a known directory.
    let f = fixture(&[("src/app/model/User.kt", "package other$0\n")]);
    assert!(f.items_with(opts()).is_empty());
    assert!(fixture(&[("src/Main.kt", "package $0\n")]).labels().is_empty());
}

#[test]
fn completes_import_stars_and_aliases() {
    let labels = completes("import kotlin.collections.$0\n");
    assert!(has(&labels, "*") && has(&labels, "List") && has(&labels, "listOf"), "{labels:?}");
    assert!(!has(&completes("import kotlin.collections.Li$0\n"), "*"));
    assert_eq!(completes("import kotlin.collections.List $0\n"), ["as"]);
    assert_eq!(completes("import kotlin.collections.List a$0\n"), ["as"]);
    // The alias is a new name.
    assert!(completes("import kotlin.collections.List as $0\n").is_empty());
}

const SEALED_SHAPES: &str = "sealed interface Shape\nclass Circle(val r: Double) : Shape\nobject Empty : Shape\nclass Square : Shape\n";
const COLORS: &str = "enum class Color { RED, GREEN, BLUE }\n";

#[test]
fn completes_the_branches_a_when_is_missing() {
    let f = fixture(&[("src/Main.kt", &format!("{COLORS}fun f(c: Color) = when (c) {{\n    Color.RED -> 1\n    $0\n}}\n"))]);
    let items = f.items();
    let labels: Vec<&str> = items.iter().map(|i| i["label"].as_str().unwrap()).collect();
    assert!(labels.contains(&"Color.GREEN") && labels.contains(&"Color.BLUE") && !labels.contains(&"Color.RED"), "{labels:?}");
    assert_eq!(item(&items, "Color.GREEN")["textEdit"]["newText"], "Color.GREEN -> ");
    assert!(labels.contains(&"else"), "something is missing, so `else` adds something: {labels:?}");
    // Typing the entry's name finds it.
    let labels = completes(&format!("{COLORS}fun f(c: Color) = when (c) {{\n    Color.RED -> 1\n    GR$0\n}}\n"));
    assert!(has(&labels, "Color.GREEN") && !has(&labels, "Color.BLUE"), "{labels:?}");

    // Everything covered: neither the cases nor `else`.
    let labels =
        completes(&format!("{COLORS}fun f(c: Color) = when (c) {{\n    Color.RED, Color.GREEN -> 1\n    Color.BLUE -> 2\n    $0\n}}\n"));
    assert!(!has(&labels, "else") && !labels.iter().any(|l| l.starts_with("Color.")), "{labels:?}");
    // After a comma, another condition of the same branch.
    let f = fixture(&[("src/Main.kt", &format!("{COLORS}fun f(c: Color) = when (c) {{\n    Color.RED, $0 -> 1\n}}\n"))]);
    assert_eq!(item(&f.items(), "Color.GREEN")["textEdit"]["newText"], "Color.GREEN");

    // A sealed type: `is` for classes, the object itself, and `null` for a nullable subject.
    let labels = completes(&format!("{SEALED_SHAPES}fun f(s: Shape?) = when (s) {{\n    is Square -> 0\n    $0\n}}\n"));
    for expected in ["is Circle", "Empty", "null"] {
        assert!(has(&labels, expected), "{expected}: {labels:?}");
    }
    assert!(!has(&labels, "is Square"), "{labels:?}");
    // `when (val x = ...)` and a Boolean.
    let labels = completes(&format!("{SEALED_SHAPES}fun s(): Shape = Empty\nfun f() = when (val x = s()) {{\n    $0\n}}\n"));
    assert!(has(&labels, "is Circle"), "{labels:?}");
    let labels = completes("fun f(b: Boolean) = when (b) {\n    true -> 1\n    $0\n}\n");
    assert!(has(&labels, "false") && has(&labels, "else"), "{labels:?}");

    // `else` once only, and freely where the subject has no known cases.
    let labels = completes(&format!("{COLORS}fun f(c: Color) = when (c) {{\n    else -> 1\n    $0\n}}\n"));
    assert!(!has(&labels, "else"), "{labels:?}");
    let labels = completes("fun f(n: Int) = when (n) {\n    1 -> 1\n    $0\n}\n");
    assert!(has(&labels, "else"), "{labels:?}");

    // A class in another package is imported with its case.
    let shapes = format!("package shapes\n\n{SEALED_SHAPES}");
    let f = fixture(&[
        ("src/shapes/Shapes.kt", &shapes),
        ("src/app/Main.kt", "package app\n\nimport shapes.Shape\n\nfun f(s: Shape) = when (s) {\n    $0\n}\n"),
    ]);
    let items = f.items();
    assert_eq!(item(&items, "is Circle")["additionalTextEdits"][0]["newText"], "import shapes.Circle\n", "{items:?}");
}

#[test]
fn adds_the_remaining_branches_of_a_when() {
    let f = fixture(&[("src/Main.kt", &format!("{COLORS}fun f(c: Color) = wh$0en (c) {{\n    Color.RED -> 1\n}}\n"))]);
    assert_eq!(
        f.applied(Vec::new(), "Add remaining branches"),
        format!(
            "{COLORS}fun f(c: Color) = when (c) {{\n    Color.RED -> 1\n    Color.GREEN -> TODO(\"Not yet implemented\")\n    Color.BLUE -> TODO(\"Not yet implemented\")\n}}\n"
        )
    );
    // Before `else`, which stays last; and into empty braces.
    let f = fixture(&[(
        "src/Main.kt",
        &format!("{SEALED_SHAPES}fun f(s: Shape) {{\n    wh$0en (s) {{\n        is Circle -> 1\n        else -> 2\n    }}\n}}\n"),
    )]);
    assert_eq!(
        f.applied(Vec::new(), "Add remaining branches"),
        format!(
            "{SEALED_SHAPES}fun f(s: Shape) {{\n    when (s) {{\n        is Circle -> 1\n        Empty -> TODO(\"Not yet implemented\")\n        is Square -> TODO(\"Not yet implemented\")\n        else -> 2\n    }}\n}}\n"
        )
    );
    let f = fixture(&[("src/Main.kt", "fun f(b: Boolean) = wh$0en (b) {}\n")]);
    assert_eq!(
        f.applied(Vec::new(), "Add remaining branches"),
        "fun f(b: Boolean) = when (b) {\n    true -> TODO(\"Not yet implemented\")\n    false -> TODO(\"Not yet implemented\")\n}\n"
    );
    let f = fixture(&[("src/Main.kt", &format!("{COLORS}fun f(c: Color) = wh$0en (c) {{\n    Color.RED, Color.GREEN -> 1\n}}\n"))]);
    assert!(f.titles(Vec::new()).contains(&"Add the missing branch `Color.BLUE`".to_owned()), "{:?}", f.titles(Vec::new()));
    // Complete, or over something without cases: no action.
    let f = fixture(&[("src/Main.kt", "fun f(b: Boolean) = wh$0en (b) {\n    true -> 1\n    false -> 2\n}\n")]);
    assert!(f.titles_but_kdoc(Vec::new()).is_empty());
    let f = fixture(&[("src/Main.kt", "fun f(n: Int) = wh$0en (n) {\n    1 -> 1\n}\n")]);
    assert!(f.titles_but_kdoc(Vec::new()).is_empty());
}

#[test]
fn completes_annotation_targets_and_arguments() {
    let decls = "annotation class Marker(val level: Int, val note: String = \"\")\nfun markerFun() = 1\n";
    let labels = completes(&format!("{decls}class A(@$0 val x: Int)\n"));
    assert!(has(&labels, "get:") && has(&labels, "field:") && has(&labels, "param:") && has(&labels, "Marker"), "{labels:?}");
    assert!(!has(&labels, "file:") && !has(&labels, "markerFun"), "{labels:?}");
    let labels = completes(&format!("@$0\n{decls}"));
    assert!(has(&labels, "file:"), "at the top of the file: {labels:?}");
    // After a target, annotation classes only.
    let labels = completes(&format!("{decls}class A(@get:Mar$0 val x: Int)\n"));
    assert!(has(&labels, "Marker") && !has(&labels, "markerFun"), "{labels:?}");
    // The parameters of a Kotlin annotation's constructor.
    let f = fixture(&[("src/Main.kt", &format!("{decls}@Marker($0)\nclass B\n"))]);
    let items = f.items();
    assert_eq!(item(&items, "level")["textEdit"]["newText"], "level = ");
    let labels = completes(&format!("{decls}@Marker(level = 1, $0)\nclass B\n"));
    assert!(has(&labels, "note") && !has(&labels, "level"), "{labels:?}");
}

#[test]
fn completes_values_in_string_templates() {
    let src = "class User(val name: String) {\n    fun greet(count: Int) = \"$na$0\"\n}\n";
    let labels = completes(src);
    assert!(has(&labels, "name"), "{labels:?}");
    let labels = completes(&src.replace("$na$0", "$c$0"));
    assert!(has(&labels, "count") && !has(&labels, "greet") && !has(&labels, "class"), "{labels:?}");
    let labels = completes(&src.replace("$na$0", "Hi $th$0"));
    assert!(has(&labels, "this"), "{labels:?}");
    // `${ }` is a full expression.
    let labels = completes(&src.replace("$na$0", "${na$0}"));
    assert!(has(&labels, "name"), "{labels:?}");
    let labels = completes(&src.replace("$na$0", "${name.$0}"));
    assert!(has(&labels, "length"), "{labels:?}");
    let labels = completes(&src.replace("$na$0", "${gr$0}"));
    assert!(has(&labels, "greet"), "{labels:?}");
}

#[test]
fn completes_supertypes_receivers_and_type_parameters() {
    let decls = "interface Shape\ninterface Named\nopen class Base(val x: Int)\nabstract class Plain\nclass Final\n";
    let f = fixture(&[("src/Main.kt", &format!("{decls}class A : $0\n"))]);
    let items = f.items();
    let labels: Vec<&str> = items.iter().map(|i| i["label"].as_str().unwrap()).collect();
    for expected in ["Shape", "Named", "Base", "Plain"] {
        assert!(labels.contains(&expected), "{expected}: {labels:?}");
    }
    assert!(!labels.contains(&"Final") && !labels.contains(&"A") && !labels.contains(&"println"), "{labels:?}");
    assert_eq!(item(&items, "Base")["textEdit"]["newText"], "Base($0)");
    assert_eq!(item(&items, "Plain")["textEdit"]["newText"], "Plain()");
    assert_eq!(item(&items, "Shape")["textEdit"]["newText"], "Shape");
    // After the first supertype, and after a constructor.
    let labels = completes(&format!("{decls}class A(val n: Int) : Base(n), $0\n"));
    assert!(has(&labels, "Shape") && !has(&labels, "Final"), "{labels:?}");
    // An interface extends interfaces only; an object expression takes classes too.
    let labels = completes(&format!("{decls}interface I : $0\n"));
    assert!(has(&labels, "Shape") && !has(&labels, "Base"), "{labels:?}");
    let labels = completes(&format!("{decls}val o = object : $0\n"));
    assert!(has(&labels, "Base") && has(&labels, "Shape"), "{labels:?}");

    // The receiver type after `fun`, and nothing after it: a new name.
    let labels = completes("fun Str$0\n");
    assert!(has(&labels, "String") && !has(&labels, "println"), "{labels:?}");
    let labels = completes("fun <T> Li$0\n");
    assert!(has(&labels, "List") && !has(&labels, "listOf"), "{labels:?}");
    assert!(completes("fun String.$0\n").is_empty());

    // Type parameters in scope, in types.
    let labels = completes("fun <Item> f(x: It$0) {}\n");
    assert!(has(&labels, "Item"), "{labels:?}");
    let labels = completes("class Box<E> {\n    fun get(): $0\n}\n");
    assert!(has(&labels, "E") && has(&labels, "String"), "{labels:?}");
    let labels = completes("val xs: List<$0>\n");
    assert!(has(&labels, "String") && !has(&labels, "println"), "{labels:?}");
    let labels = completes("fun <Item> f(xs: List<It$0>) {}\n");
    assert!(has(&labels, "Item"), "{labels:?}");
}

#[test]
fn new_names_get_nothing() {
    for src in [
        "val $0",
        "fun f() {\n    var co$0\n}\n",
        "class $0",
        "interface Sh$0",
        "object $0",
        "typealias $0",
        "fun <$0",
        "class Box<$0",
        "fun f() {\n    try {} catch ($0\n}\n",
        "fun f() {\n    for ($0\n}\n",
    ] {
        assert!(completes(src).is_empty(), "{src}: {:?}", completes(src));
    }
    // A parameter's name: only the modifiers that fit before it.
    assert_eq!(completes("fun f($0"), ["vararg", "noinline", "crossinline"]);
    assert_eq!(completes("fun f(a: Int, v$0"), ["vararg"]);
    let labels = completes("class A($0");
    assert!(has(&labels, "val") && has(&labels, "var") && has(&labels, "private") && !has(&labels, "println"), "{labels:?}");
    let labels = completes("class A private constructor(v$0");
    assert!(has(&labels, "val"), "{labels:?}");
    assert_eq!(completes("class A {\n    constructor(v$0\n}\n"), ["vararg"]);
    // Arguments of calls are expressions still.
    assert!(has(&completes("fun f(a: Int) = a\nfun g() = f($0"), "a ="));
}

#[test]
fn completes_class_body_keywords() {
    let labels = completes("class A {\n    $0\n}\n");
    for expected in ["constructor", "init", "companion object", "fun", "val"] {
        assert!(has(&labels, expected), "{expected}: {labels:?}");
    }
    let labels = completes("interface I {\n    $0\n}\n");
    assert!(!has(&labels, "constructor") && !has(&labels, "init") && has(&labels, "companion object"), "{labels:?}");
    let labels = completes("object O {\n    $0\n}\n");
    assert!(!has(&labels, "constructor") && has(&labels, "init") && !has(&labels, "companion object"), "{labels:?}");
    let labels = completes("class A {\n    companion object {}\n    $0\n}\n");
    assert!(!has(&labels, "companion object"), "{labels:?}");
    // Not in functions.
    let labels = completes("class A {\n    fun f() {\n        $0\n    }\n}\n");
    assert!(!has(&labels, "constructor") && !has(&labels, "init"), "{labels:?}");
}

#[test]
fn completes_labels_in_scope() {
    let labels = completes("fun f() {\n    listOf(1).forEach {\n        return@$0\n    }\n}\n");
    assert_eq!(labels, ["forEach"]);
    let labels = completes("fun f() {\n    run outer@{\n        listOf(1).map { return@o$0 }\n    }\n}\n");
    assert_eq!(labels, ["outer"]);
    let labels = completes(
        "fun f() {\n    loop@ for (i in 1..2) {\n        inner@ while (true) {\n            break@$0\n        }\n    }\n    done@ for (j in 1..2) {}\n}\n",
    );
    assert_eq!(labels, ["loop", "inner"]);
    let labels = completes("fun f() {\n    loop@ for (i in 1..2) {\n        continue@l$0\n    }\n}\n");
    assert_eq!(labels, ["loop"]);
    let labels = completes("class Outer {\n    inner class Inner {\n        fun String.f() = this@$0\n    }\n}\n");
    assert_eq!(labels, ["f", "Inner", "Outer"]);
    let labels = completes("fun f() {\n    buildString {\n        this@$0\n    }\n}\n");
    assert_eq!(labels, ["buildString"]);
    // No labels: nothing, not annotations.
    assert!(completes("fun f() {\n    break@$0\n}\n").is_empty());
}

#[test]
fn completes_lambda_parameter_names() {
    let decl = "fun call(block: (String, Int) -> Unit) {}\n";
    let f = fixture(&[("src/Main.kt", &format!("{decl}fun main() {{\n    call {{ $0 }}\n}}\n"))]);
    let items = f.items();
    assert_eq!(item(&items, "string, int ->")["textEdit"]["newText"], "string, int -> ");
    let labels = completes(&format!("{decl}fun main() {{\n    call {{ s$0 }}\n}}\n"));
    assert!(has(&labels, "string, int ->"), "{labels:?}");
    let labels = completes("fun main() {\n    listOf(\"a\").map { $0 }\n}\n");
    assert!(has(&labels, "string ->"), "{labels:?}");
    // A lambda without parameters gets none.
    let labels = completes("fun main() {\n    run { $0 }\n}\n");
    assert!(!labels.iter().any(|l| l.ends_with("->")), "{labels:?}");
}

mod refactoring {
    //! The refactorings of [`super::super::refactor`]: each test applies an action's edit and
    //! compares the text.

    use std::collections::BTreeMap;
    use std::ops::Range;

    use super::*;

    /// A fixture of `files`, and the selection of its cursor file, from `$0` to `$1` (empty at
    /// `$0` without a `$1`).
    fn selected(files: &[(&str, &str)]) -> (Fixture, Range<usize>) {
        let mut end = None;
        let owned: Vec<(String, String)> = files
            .iter()
            .map(|(path, text)| match (text.find("$0"), text.find("$1")) {
                (Some(start), Some(at)) => {
                    end = Some(if start < at { at - 2 } else { at });
                    (path.to_string(), text.replacen("$1", "", 1))
                }
                _ => (path.to_string(), text.to_string()),
            })
            .collect();
        let refs: Vec<(&str, &str)> = owned.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
        let f = fixture(&refs);
        let (_, start) = f.cursor();
        (f, start..end.unwrap_or(start))
    }

    fn actions(files: &[(&str, &str)]) -> (Fixture, Vec<Value>) {
        let (f, range) = selected(files);
        let (unit, _) = f.cursor();
        let manifests = vec![PathBuf::from("/project/kargo.toml")];
        let request = actions::Request {
            unit,
            range,
            only: vec!["refactor".into()],
            diagnostics: Vec::new(),
            roots: &[],
            manifests: &manifests,
            document_changes: true,
            create_files: true,
        };
        let found = actions::code_actions(&f.an(), &request);
        (f, found)
    }

    fn titles(files: &[(&str, &str)]) -> Vec<String> {
        actions(files).1.iter().map(|a| a["title"].as_str().unwrap().to_owned()).collect()
    }

    /// The files the refactoring titled `title` changes or creates, by path in the project,
    /// with its edit applied.
    fn refactored(files: &[(&str, &str)], title: &str) -> BTreeMap<String, String> {
        let (f, found) = actions(files);
        let action = found.iter().find(|a| a["title"] == title).unwrap_or_else(|| panic!("no `{title}` in {found:#?}"));
        assert!(action["kind"].as_str().unwrap().starts_with("refactor."), "{action}");
        let mut out = BTreeMap::new();
        for change in action["edit"]["documentChanges"].as_array().expect("documentChanges") {
            if change["kind"] == "create" {
                continue;
            }
            let uri = change["textDocument"]["uri"].as_str().unwrap();
            let original =
                f.proj.units.iter().find(|u| crate::text::path_to_uri(&u.path) == uri).map(|u| u.text.clone()).unwrap_or_default();
            let path = uri.split("/project/").nth(1).unwrap().to_owned();
            // An editor rejects edits of one file that overlap or insert at the same place.
            let mut spans: Vec<(usize, usize)> = change["edits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| (crate::text::offset(&original, &e["range"]["start"]), crate::text::offset(&original, &e["range"]["end"])))
                .collect();
            spans.sort();
            assert!(spans.windows(2).all(|w| w[0].1 <= w[1].0 && w[0] != w[1]), "overlapping edits of {path}: {spans:?}");
            out.insert(path, apply_edits(&original, change["edits"].as_array().unwrap()));
        }
        out
    }

    /// The cursor file after the refactoring titled `title`.
    fn main_after(files: &[(&str, &str)], title: &str) -> String {
        let path = files.iter().find(|(_, t)| t.contains("$0")).unwrap().0;
        refactored(files, title).remove(path).expect("the cursor file changes")
    }

    #[test]
    fn extracts_a_variable() {
        let main = "fun main() {\n    val items = listOf(1, 2, 3)\n    println($0items.size$1 * 2 + 1)\n}\n";
        assert_eq!(
            main_after(&[("src/Main.kt", main)], "Extract variable"),
            "fun main() {\n    val items = listOf(1, 2, 3)\n    val size: Int = items.size\n    println(size * 2 + 1)\n}\n"
        );

        // Every equal expression of the block, on request.
        let area = "fun area(w: Int, h: Int): Int {\n    println(w * h)\n    return $0w * h$1 + 1\n}\n";
        assert_eq!(
            main_after(&[("src/Main.kt", area)], "Extract variable (replace all 2 occurrences)"),
            "fun area(w: Int, h: Int): Int {\n    val int: Int = w * h\n    println(int)\n    return int + 1\n}\n"
        );
        assert_eq!(
            main_after(&[("src/Main.kt", area)], "Extract variable"),
            "fun area(w: Int, h: Int): Int {\n    println(w * h)\n    val int: Int = w * h\n    return int + 1\n}\n"
        );

        // From the condition of an `if`, which always runs; inside a lambda, into the lambda.
        let condition = "fun main(x: Int) {\n    if ($0x * 2$1 > 3) println(x)\n}\n";
        assert_eq!(
            main_after(&[("src/Main.kt", condition)], "Extract variable"),
            "fun main(x: Int) {\n    val int: Int = x * 2\n    if (int > 3) println(x)\n}\n"
        );
        let lambda = "fun main() {\n    listOf(1).forEach { println($0it + 1$1) }\n}\n";
        assert_eq!(
            main_after(&[("src/Main.kt", lambda)], "Extract variable"),
            "fun main() {\n    listOf(1).forEach { val int: Int = it + 1; println(int) }\n}\n"
        );

        // Not part of a larger operand, nor code that may not run, nor a whole statement.
        let none = |main: &str| !titles(&[("src/Main.kt", main)]).iter().any(|t| t.starts_with("Extract variable"));
        assert!(none("fun main(x: Int) {\n    if (x > 3) println($0x * 2$1)\n}\n"));
        assert!(none("fun main() {\n    val x = 1\n    println(x * $0x + 1$1)\n}\n"));
        assert!(none("fun check(s: String) = s.isEmpty()\nfun main(flag: Boolean) {\n    val x = flag && $0check(\"a\")$1\n}\n"));
        assert!(none("fun main() {\n    $0println(1)$1\n}\n"));
        assert!(none("fun main() {\n    val x = $0unknown()$1\n}\n"));
    }

    #[test]
    fn extracts_a_function() {
        let main = "fun report(items: List<Int>) {\n    val prefix = \"total\"\n    $0val total = items.size\n    println(\"$prefix: $total\")$1\n    println(total * 2)\n}\n";
        assert_eq!(
            main_after(&[("src/Main.kt", main)], "Extract function"),
            "fun report(items: List<Int>) {\n    val prefix = \"total\"\n    val total = extracted(items, prefix)\n    println(total * 2)\n}\n\nprivate fun extracted(items: List<Int>, prefix: String): Int {\n    val total = items.size\n    println(\"$prefix: $total\")\n    return total\n}\n"
        );

        // A member stays a member, and nothing is returned when nothing is used after.
        let class =
            "class Greeter(val name: String) {\n    fun greet(times: Int) {\n        $0repeat(times) { println(name) }$1\n    }\n}\n";
        assert_eq!(
            main_after(&[("src/Main.kt", class)], "Extract function"),
            "class Greeter(val name: String) {\n    fun greet(times: Int) {\n        extracted(times)\n    }\n\n    private fun extracted(times: Int) {\n        repeat(times) { println(name) }\n    }\n}\n"
        );

        // Not with a `return`, nor with two results, nor assigning a local of the function.
        let none = |main: &str| !titles(&[("src/Main.kt", main)]).iter().any(|t| t == "Extract function");
        assert!(none("fun f(x: Int): Int {\n    $0if (x > 0) return 1$1\n    return 0\n}\n"));
        assert!(none("fun f() {\n    $0val a = 1\n    val b = 2$1\n    println(a + b)\n}\n"));
        assert!(none("fun f() {\n    var n = 0\n    $0n = 2$1\n    println(n)\n}\n"));
    }

    #[test]
    fn inlines_a_variable() {
        let main = "fun main() {\n    val sum$0 = 1 + 2\n    println(sum * 3)\n    println(\"$sum\")\n}\n";
        assert_eq!(
            main_after(&[("src/Main.kt", main)], "Inline variable `sum`"),
            "fun main() {\n    println((1 + 2) * 3)\n    println(\"${1 + 2}\")\n}\n"
        );

        // A call moves only to a single use that runs first in the next statement.
        let next = "fun next(): Int = 1\n\nfun main() {\n    val n = next()\n    println(n$0)\n}\n";
        assert_eq!(
            main_after(&[("src/Main.kt", next)], "Inline variable `n`"),
            "fun next(): Int = 1\n\nfun main() {\n    println(next())\n}\n"
        );
        let none = |main: &str| !titles(&[("src/Main.kt", main)]).iter().any(|t| t.starts_with("Inline variable"));
        assert!(none("fun next(): Int = 1\n\nfun main() {\n    val n$0 = next()\n    println(1)\n    println(n)\n}\n"));
        assert!(none("fun next(): Int = 1\n\nfun main() {\n    val n$0 = next()\n    println(n + n)\n}\n"));
        // Nor a `var`.
        assert!(none("fun main() {\n    var n$0 = 1\n    println(n)\n}\n"));
    }

    #[test]
    fn inlines_a_function() {
        let main = "fun square(x: Int) = x * x\n\nfun main() {\n    println(squ$0are(3) + 1)\n    println(square(2 + 1))\n}\n";
        assert_eq!(
            main_after(&[("src/Main.kt", main)], "Inline function `square`"),
            "fun main() {\n    println(3 * 3 + 1)\n    println((2 + 1) * (2 + 1))\n}\n"
        );

        // In another package, with the imports the body needs.
        let lib = "package lib\n\nval suffix = \"!\"\n\nfun gre$0et(name: String): String {\n    return \"Hello, \" + name + suffix\n}\n";
        let app = "package app\n\nimport lib.greet\n\nfun main() {\n    println(greet(\"Ana\").length)\n}\n";
        let files = refactored(&[("src/lib/Greet.kt", lib), ("src/app/Main.kt", app)], "Inline function `greet`");
        assert_eq!(files["src/lib/Greet.kt"], "package lib\n\nval suffix = \"!\"\n");
        assert_eq!(
            files["src/app/Main.kt"],
            "package app\n\nimport lib.suffix\n\nfun main() {\n    println((\"Hello, \" + \"Ana\" + suffix).length)\n}\n"
        );

        // A `Unit` function of one statement, where it is called as a statement.
        let log = "fun log(message: String) {\n    println(message)\n}\n\nfun main() {\n    lo$0g(\"a\")\n}\n";
        assert_eq!(main_after(&[("src/Main.kt", log)], "Inline function `log`"), "fun main() {\n    println(\"a\")\n}\n");

        // Not used as a value (`::square`), nor with an argument whose side effects would repeat.
        let none = |main: &str| !titles(&[("src/Main.kt", main)]).iter().any(|t| t.starts_with("Inline function"));
        assert!(none("fun square(x: Int) = x * x\n\nfun main() {\n    println(listOf(1).map(::squ$0are))\n}\n"));
        assert!(none("fun next(): Int = 1\nfun square(x: Int) = x * x\n\nfun main() {\n    println(squ$0are(next()))\n}\n"));
    }

    #[test]
    fn changes_a_signature() {
        let main = "fun describe(name: String, ag$0e: Int) = \"$name is $age\"\n\nfun main() {\n    println(describe(\"Ana\", 30))\n    println(describe(name = \"Bo\", age = 4))\n}\n";
        let found = titles(&[("src/Main.kt", main)]);
        assert!(
            found.contains(&"Move parameter `age` left".to_owned()) && !found.iter().any(|t| t.starts_with("Remove parameter")),
            "{found:?}"
        );
        assert_eq!(
            main_after(&[("src/Main.kt", main)], "Move parameter `age` left"),
            "fun describe(age: Int, name: String) = \"$name is $age\"\n\nfun main() {\n    println(describe(30, \"Ana\"))\n    println(describe(name = \"Bo\", age = 4))\n}\n"
        );
        // A positional argument before a named one gets its name.
        let mixed =
            "fun describe(name: String, ag$0e: Int) = \"$name is $age\"\n\nfun main() {\n    println(describe(\"Ana\", age = 30))\n}\n";
        assert_eq!(
            main_after(&[("src/Main.kt", mixed)], "Move parameter `age` left"),
            "fun describe(age: Int, name: String) = \"$name is $age\"\n\nfun main() {\n    println(describe(name = \"Ana\", age = 30))\n}\n"
        );

        // Overrides change with the function, and a parameter no body uses goes.
        let shapes = "interface Shape {\n    fun draw(scale: Int, colo$0r: String)\n}\n\nclass Square : Shape {\n    override fun draw(scale: Int, color: String) {\n        println(scale)\n    }\n}\n\nfun main() {\n    Square().draw(2, \"red\")\n}\n";
        assert_eq!(
            main_after(&[("src/Main.kt", shapes)], "Remove parameter `color`"),
            "interface Shape {\n    fun draw(scale: Int)\n}\n\nclass Square : Shape {\n    override fun draw(scale: Int) {\n        println(scale)\n    }\n}\n\nfun main() {\n    Square().draw(2)\n}\n"
        );

        // Not when an argument it would drop has side effects, nor for a callable reference.
        let none = |main: &str, prefix: &str| !titles(&[("src/Main.kt", main)]).iter().any(|t| t.starts_with(prefix));
        assert!(none(
            "fun next() = \"\"\nfun log(message: String, le$0vel: String) = println(message)\n\nfun main() {\n    log(\"a\", next())\n}\n",
            "Remove"
        ));
        assert!(none("fun add(a: Int, b$0: Int) = a + b\n\nfun main() {\n    println(listOf(1).fold(0, ::add))\n}\n", "Move"));
    }

    #[test]
    fn moves_a_declaration() {
        let util = "package app\n\nval greeting = \"Hi\"\n\nclass Gr$0eeter {\n    fun greet() = greeting\n}\n";
        let main = "package app\n\nfun main() {\n    println(Greeter().greet())\n}\n";
        let user = "package app.model\n\nclass User\n";
        let other = "package other\n\nimport app.Greeter\n\nfun make(): Greeter = app.Greeter()\n";
        let files = [("src/app/Util.kt", util), ("src/app/Main.kt", main), ("src/app/model/User.kt", user), ("src/other/Use.kt", other)];
        let found = titles(&files);
        assert!(found.contains(&"Move `Greeter` to package `other`".to_owned()), "{found:?}");
        let moved = refactored(&files, "Move `Greeter` to package `app.model`");
        assert_eq!(moved["src/app/Util.kt"], "package app\n\nval greeting = \"Hi\"\n");
        assert_eq!(
            moved["src/app/model/Greeter.kt"],
            "package app.model\n\nimport app.greeting\n\nclass Greeter {\n    fun greet() = greeting\n}\n"
        );
        assert_eq!(
            moved["src/app/Main.kt"],
            "package app\n\nimport app.model.Greeter\n\nfun main() {\n    println(Greeter().greet())\n}\n"
        );
        assert_eq!(moved["src/other/Use.kt"], "package other\n\nimport app.model.Greeter\n\nfun make(): Greeter = app.model.Greeter()\n");

        // To a file of its own.
        let own = refactored(&files, "Move `Greeter` to `Greeter.kt`");
        assert_eq!(own["src/app/Greeter.kt"], "package app\n\nclass Greeter {\n    fun greet() = greeting\n}\n");
        assert_eq!(own.len(), 2, "{own:?}");

        // Not what uses a private declaration of its file.
        let private = "package app\n\nprivate val secret = 1\n\nfun sh$0ow() = secret\n";
        assert!(!titles(&[("src/app/Util.kt", private), ("src/app/model/User.kt", user)]).iter().any(|t| t.starts_with("Move")));
    }
}

#[test]
fn marks_uses_of_deprecated_declarations() {
    let code = "package app\n\n@Deprecated(\"use fresh\", ReplaceWith(\"fresh()\"))\nfun old() {}\n\n@Deprecated(\"gone\", level = DeprecationLevel.ERROR)\nclass Gone\n\n@Deprecated(\"hidden\", level = DeprecationLevel.HIDDEN)\nfun secret() {}\n\nfun fresh() {}\n\nfun main() {\n    old()\n    fresh()\n    Gone()\n    secret()\n}\n";
    let f = fixture(&[("src/Main.kt", code)]);
    let an = f.an();
    let problems = super::diagnostics::deprecated_usages(&an, UnitId { lib: false, unit: 0 });
    let found: Vec<(&str, Severity, Vec<Tag>, &str)> =
        problems.iter().map(|p| (&code[p.span.clone()], p.severity, p.tags.clone(), p.message.as_str())).collect();
    assert_eq!(
        found,
        [
            ("old", Severity::Hint, vec![Tag::Deprecated], "`old` is deprecated: use fresh. Use `fresh()` instead"),
            ("Gone", Severity::Error, vec![Tag::Deprecated], "`Gone` is deprecated: gone"),
        ],
    );
}

#[test]
fn intermediate_source_sets_see_the_code_of_their_targets() {
    let set = |target: Option<String>| SourceSet { package: 0, kind: SetKind::Main, target };
    let common = set(None);
    let jvm = set(Some("jvm".into()));
    let android = set(Some("android".into()));
    let wasm = set(Some("wasm".into()));
    let jvm_android = set(Some(SourceSet::group_target("jvmAndAndroid", &["jvm", "android"], &[])));
    let non_web = set(Some(SourceSet::group_target("nonWeb", &["jvm", "android", "native"], &[])));

    // Everything sees common code; only the targets of a set see it.
    for from in [&common, &jvm, &android, &wasm, &jvm_android, &non_web] {
        assert!(common.visible_from(from));
    }
    assert!(jvm_android.visible_from(&jvm) && jvm_android.visible_from(&android));
    assert!(!jvm_android.visible_from(&wasm) && !jvm_android.visible_from(&common));
    // A set sees the sets that hold at least its targets, but not the other way around.
    assert!(non_web.visible_from(&jvm_android));
    assert!(!jvm_android.visible_from(&non_web));
    // A target's own code is for that target alone.
    assert!(jvm.visible_from(&jvm) && !jvm.visible_from(&android) && !jvm.visible_from(&jvm_android) && !jvm.visible_from(&common));
    assert_eq!((jvm.own_target(), jvm_android.own_target(), jvm_android.some_target()), (Some("jvm"), None, Some("jvm")));

    // A set compiles for all its targets, so it sees only the libraries all of them have.
    let lib = Index::new(true, Vec::new());
    let proj = Index::new(false, Vec::new());
    let packages = Packages { visible: vec![vec![0]], platforms: vec![Platforms::ALL] };
    let an = Analyzer::new(&lib, &proj, &packages);
    let platforms = |set: &SourceSet| an.platforms_of(set);
    assert_eq!(platforms(&jvm_android), Platforms::JVM);
    assert_eq!(platforms(&non_web), Platforms::JVM.union(Platforms::NATIVE));
}

#[test]
fn sets_of_native_platforms_sit_below_the_native_directory() {
    let set = |target: Option<String>| SourceSet { package: 0, kind: SetKind::Main, target };
    let native = set(Some("native".into()));
    let common = set(None);
    let jvm = set(Some("jvm".into()));
    let non_js = set(Some(SourceSet::group_target("nonJs", &["jvm", "native"], &[])));
    let apple = set(Some(SourceSet::group_target("apple", &["native"], &["ios_arm64", "macos_arm64"])));
    let ios = set(Some(SourceSet::group_target("ios", &["native"], &["ios_arm64"])));
    assert_eq!(apple.group(), Some(("apple", vec!["native"])));
    assert_eq!(apple.group_platforms(), ["ios_arm64", "macos_arm64"]);
    assert!(non_js.group_platforms().is_empty());

    // `src-native/` and the sets of whole targets are above a set of platforms, and common code cannot see it.
    for above in [&common, &native, &non_js] {
        assert!(above.visible_from(&apple) && above.visible_from(&ios));
    }
    assert!(!jvm.visible_from(&apple));
    assert!(!apple.visible_from(&common) && !apple.visible_from(&native) && !apple.visible_from(&non_js));
    // A set sees the sets of more platforms, not of fewer.
    assert!(apple.visible_from(&ios) && !ios.visible_from(&apple));
    assert!(apple.visible_from(&apple));
}
