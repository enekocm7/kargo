//! Tests of the Kotlin analysis against the built-in outline of the standard library.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

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
        },
        SourcesJar {
            name: "kotlin-stdlib-wasm-js".into(),
            jar: PathBuf::from(std::env::var("KOTLIN_WASM_SOURCES").expect("KOTLIN_WASM_SOURCES")),
            main: Platforms::ALL,
            test: Platforms::NONE,
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
