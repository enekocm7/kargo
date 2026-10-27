//! The `kargo/testTree` request: the test classes and test functions of the project's test
//! sources, each with the `kargo test` command line that runs it, for editors without a test
//! plugin of their own.

use std::path::Path;

use serde_json::{Value, json};

use super::index::Unit;
use super::syntax::{DeclId, File, Kind};
use crate::text;

/// The annotations that make a function a test: kotlin.test's and JUnit's.
const TEST_ANNOTATIONS: [&str; 6] = ["@Test", "@ParameterizedTest", "@RepeatedTest", "@TestFactory", "@TestTemplate", "@Property"];

/// A test file: its unit, the target of its source set (`None` for `tests/`) and the directory of
/// its package, where `kargo test` runs.
pub struct TestFile<'a> {
    pub unit: &'a Unit,
    pub target: Option<&'a str>,
    pub cwd: Option<&'a Path>,
}

/// The tests of `files`, as one entry per file that has any:
/// `{ uri, target, tests: [{ id, label, kind: "class", range, args, children: [{ id, label, kind: "test", range, args }] }] }`.
/// `args` are the arguments of `program` (`kargo`) that run the test, in `cwd`.
pub fn test_tree(files: &[TestFile], program: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for f in files {
        let (src, file) = (&f.unit.text, &f.unit.file);
        let classes: Vec<Value> = file.top.iter().filter_map(|&id| class_entry(f, src, file, id, program)).collect();
        if !classes.is_empty() {
            out.push(json!({ "uri": text::path_to_uri(&f.unit.path), "target": f.target, "tests": classes }));
        }
    }
    out
}

/// A test class with its test functions and nested test classes, or `None` when it has none.
fn class_entry(f: &TestFile, src: &str, file: &File, id: DeclId, program: &str) -> Option<Value> {
    let decl = &file.decls[id];
    if !matches!(decl.kind, Kind::Class | Kind::Object) || decl.has("abstract") || decl.generated {
        return None;
    }
    let fq = file.fq_name(id);
    let mut children = Vec::new();
    for &m in &decl.members {
        let member = &file.decls[m];
        if member.kind == Kind::Function && TEST_ANNOTATIONS.iter().any(|a| member.has(a)) {
            let id = format!("{fq}.{}", member.name);
            children.push(entry(f, src, &id, &member.name, "test", member.span.clone(), program));
        } else if let Some(nested) = class_entry(f, src, file, m, program) {
            children.push(nested);
        }
    }
    if children.is_empty() {
        return None;
    }
    let mut class = entry(f, src, &fq, &decl.name, "class", decl.span.clone(), program);
    class["children"] = json!(children);
    Some(class)
}

fn entry(f: &TestFile, src: &str, id: &str, label: &str, kind: &str, span: std::ops::Range<usize>, program: &str) -> Value {
    let mut args = vec!["test".to_owned()];
    if let Some(target) = f.target {
        args.extend(["-t".to_owned(), target.to_owned()]);
    }
    // `kargo test` runs the tests whose name contains the filter: `.Class.` matches the class's
    // own tests and its nested classes', not those of `ClassTest`.
    let filter = if kind == "class" { format!("{id}.") } else { id.to_owned() };
    args.push(filter);
    let mut value = json!({
        "id": id,
        "label": label,
        "kind": kind,
        "range": text::range(src, span),
        "cwd": f.cwd.map(|p| p.display().to_string()),
        "program": program,
        "args": args,
    });
    if matches!(f.target, None | Some("jvm")) {
        let mut debug = args.clone();
        debug.push("--debug".to_owned());
        value["debugArgs"] = json!(debug);
    }
    value
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::kotlin::index::{Origin, SetKind, SourceSet};

    #[test]
    fn lists_test_classes_and_functions() {
        let src = "package app\n\nimport kotlin.test.Test\n\nclass MathTest {\n    @Test\n    fun adds() {}\n\n    fun helper() {}\n\n    class Nested {\n        @Test fun inner() {}\n    }\n}\n\nclass Helper\n\nabstract class Base {\n    @Test fun inherited() {}\n}\n";
        let set = SourceSet { package: 0, kind: SetKind::Test, target: Some("jvm".into()) };
        let unit = Unit::new(PathBuf::from("/p/tests-jvm/MathTest.kt"), src.to_owned(), Origin::Source(set));
        let cwd = PathBuf::from("/p");
        let tree = test_tree(&[TestFile { unit: &unit, target: Some("jvm"), cwd: Some(&cwd) }], "kargo");
        assert_eq!(tree.len(), 1, "{tree:?}");
        let classes = tree[0]["tests"].as_array().unwrap();
        assert_eq!(classes.len(), 1, "{tree:?}");
        let class = &classes[0];
        assert_eq!(class["id"], "app.MathTest");
        assert_eq!(class["args"], json!(["test", "-t", "jvm", "app.MathTest."]));
        let children: Vec<&str> = class["children"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(children, ["app.MathTest.adds", "app.MathTest.Nested"]);
        let adds = &class["children"][0];
        assert_eq!(adds["kind"], "test");
        assert_eq!(adds["args"], json!(["test", "-t", "jvm", "app.MathTest.adds"]));
        assert_eq!(adds["debugArgs"], json!(["test", "-t", "jvm", "app.MathTest.adds", "--debug"]));
        assert_eq!(adds["range"]["start"], json!({ "line": 5, "character": 4 }));
        assert_eq!(class["children"][1]["children"][0]["id"], "app.MathTest.Nested.inner");
    }
}
