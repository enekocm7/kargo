//! Tests of the Kotlin analysis against the built-in outline of the standard library.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use super::actions;
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
        let list = features::completion(&self.an(), u, offset, CompletionOptions { snippets: true }, 0);
        list["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_owned()).collect()
    }

    fn items(&self) -> Vec<Value> {
        let (u, offset) = self.cursor();
        features::completion(&self.an(), u, offset, CompletionOptions { snippets: true }, 0)["items"].as_array().unwrap().clone()
    }

    /// The token at the cursor.
    fn token(&self) -> (UnitId, usize) {
        let (u, offset) = self.cursor();
        let tok = self.proj.units[u.unit as usize].file.lexed.ident_at(offset).expect("an identifier at the cursor");
        (u, tok)
    }

    fn hover(&self) -> String {
        let (u, tok) = self.token();
        features::hover(&self.an(), u, tok).map(|h| h["contents"]["value"].as_str().unwrap().to_owned()).unwrap_or_default()
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
    assert!(!has(&labels, "println"), "{labels:?}");

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
    for name in ["count", "name", "prefix", "greet", "other", "println", "listOf", "Greeter", "String", "return", "val"] {
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
    let list = features::completion(&f.an(), u, offset, CompletionOptions { snippets: false }, 0);
    let plain = keyword(list["items"].as_array().unwrap(), "while");
    assert_eq!(plain["textEdit"]["newText"], "while");
    assert_eq!(plain["insertTextFormat"], 1);

    let f = fixture(&[("src/Main.kt", "fun main() {\n    whi$0(true)\n}\n")]);
    let items = f.items();
    assert_eq!(keyword(&items, "while")["textEdit"]["newText"], "while");
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
        let list = features::completion(&an, UnitId { lib: false, unit: 0 }, at, CompletionOptions { snippets: true }, 0);
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
    let found = features::workspace_symbols(&an, "mai");
    assert_eq!(found[0]["name"], "main");
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
        let list = features::completion(&an, UnitId { lib: false, unit: 0 }, at, CompletionOptions { snippets: true }, 0);
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
    super::compiled::render_classes(super::compiled::testing::sample_classes(), "generated")
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
    assert!(f.titles(Vec::new()).is_empty());
    let diagnostic = f.diagnostic("Unresolved reference 'answer'.");
    let found = f.actions(vec![diagnostic.clone()], &[]);
    assert_eq!(found[0]["title"], "Import `lib.answer`");
    assert_eq!(found[0]["diagnostics"][0], diagnostic);

    // Resolved names get nothing.
    let f = fixture(&[lib, ("src/app/Main.kt", "package app\n\nimport lib.Thing\n\nfun main() = Thi$0ng()\n")]);
    assert!(f.titles(Vec::new()).is_empty());
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
    let titles: Vec<&str> = found.iter().map(|a| a["title"].as_str().unwrap()).collect();
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
