//! Kargo scripts in the language server: a `.kts` file that `kargo script` runs. The server answers
//! completion and hover on its `@file:` annotations and reports the problems of its header as
//! diagnostics. Everything else in the file goes to the Kotlin server, as for any other Kotlin file.
//!
//! Coordinate and version completion of `@file:DependsOn` and `@file:Kotlin` use the network, so
//! the server runs them on worker threads through [`crate::deps`].

use std::ops::Range;
use std::path::Path;

use serde_json::{Value, json};

use kargo_manifest::script::ANNOTATIONS;
use kargo_ops::builtins::FRAMEWORK_NAMES;

use crate::text;

const KIND_CLASS: u32 = 7;
const KIND_VALUE: u32 = 12;
/// The prefix of every script annotation.
const PREFIX: &str = "@file:";
/// The JDKs offered for `@file:Jdk`.
const JDKS: [&str; 6] = ["system", "8", "11", "17", "21", "25"];
/// The repositories offered for `@file:Repository`.
const REPOSITORIES: [&str; 3] = ["mavenCentral", "https://jitpack.io", "https://maven.google.com"];

/// Whether `uri` names a Kotlin script file.
pub fn is_script_uri(uri: &str) -> bool {
    uri.ends_with(".kts")
}

/// Whether the `.kts` file at `path`, with `text`, is a Kargo script rather than another tool's
/// build script inside a project (see `kargo_manifest::script::is_kargo_script`).
pub fn is_kargo_script(path: &Path, text: &str) -> bool {
    let in_project = path.ancestors().skip(1).any(|dir| dir.join(kargo_manifest::MANIFEST_NAME).is_file());
    kargo_manifest::script::is_kargo_script(text, in_project)
}

/// What the cursor is on in an `@file:` annotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The annotation name after `@file:`; `range` is the name typed so far.
    Name { partial: String, range: Range<usize> },
    /// The string argument of `@file:<name>(`; `range` is the text typed so far between its quotes.
    Value { name: String, partial: String, range: Range<usize> },
}

/// How the server answers a completion or hover on a script.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// The result, answered at once.
    Now(Value),
    /// A worker thread answers later.
    Later,
    /// Not an `@file:` position: the Kotlin server answers.
    Backend,
}

/// The annotation on the line of `offset`: its name and where the name is, and the offset of the
/// opening quote of its string argument when the cursor is inside one.
struct Annotation<'a> {
    name: &'a str,
    name_span: Range<usize>,
    open_quote: Option<usize>,
}

fn scan(text: &str, offset: usize) -> Option<Annotation<'_>> {
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let at = line_start + text[line_start..offset].rfind(PREFIX)?;
    let name_start = at + PREFIX.len();
    let name_len: usize = text[name_start..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum();
    let name_end = name_start + name_len;
    let mut open_quote = None;
    if name_end < offset && text[name_end..].starts_with('(') {
        // Strings toggle; an escape inside one keeps the next quote from closing it.
        let mut escaped = false;
        for (i, c) in text[name_end + 1..offset].char_indices() {
            if escaped {
                escaped = false;
            } else if c == '\\' && open_quote.is_some() {
                escaped = true;
            } else if c == '"' {
                open_quote = match open_quote {
                    Some(_) => None,
                    None => Some(name_end + 1 + i),
                };
            }
        }
    }
    Some(Annotation { name: &text[name_start..name_end], name_span: name_start..name_end, open_quote })
}

/// The `@file:` request at byte `offset`: the name being typed, or the string argument being typed.
pub fn request(text: &str, offset: usize) -> Option<Request> {
    let annotation = scan(text, offset)?;
    if let Some(quote) = annotation.open_quote {
        return Some(Request::Value {
            name: annotation.name.to_owned(),
            partial: text[quote + 1..offset].to_owned(),
            range: quote + 1..offset,
        });
    }
    (annotation.name_span.end == offset)
        .then(|| Request::Name { partial: text[annotation.name_span.start..offset].to_owned(), range: annotation.name_span.start..offset })
}

/// The completion items of a request the server answers without the network: the annotation names,
/// and the values of `@file:Jdk`, `@file:Framework` and `@file:Repository`.
pub fn completion(text: &str, request: &Request) -> Vec<Value> {
    match request {
        Request::Name { range, .. } => ANNOTATIONS.iter().map(|(name, doc)| item(text, range.clone(), name, KIND_CLASS, doc)).collect(),
        Request::Value { name, range, .. } => match name.as_str() {
            "Jdk" => JDKS.iter().map(|jdk| item(text, range.clone(), jdk, KIND_VALUE, "JDK")).collect(),
            "Framework" => FRAMEWORK_NAMES.iter().map(|f| item(text, range.clone(), f, KIND_VALUE, "built-in framework")).collect(),
            "Repository" => REPOSITORIES.iter().map(|r| item(text, range.clone(), r, KIND_VALUE, "Maven repository")).collect(),
            _ => Vec::new(),
        },
    }
}

fn item(text: &str, range: Range<usize>, label: &str, kind: u32, detail: &str) -> Value {
    json!({
        "label": label,
        "kind": kind,
        "detail": detail,
        "textEdit": { "range": text::range(text, range), "newText": label },
    })
}

/// The hover of an annotation name at `offset`.
pub fn hover(text: &str, offset: usize) -> Option<Value> {
    let annotation = scan(text, offset)?;
    if offset < annotation.name_span.start || offset > annotation.name_span.end {
        return None;
    }
    let (name, doc) = ANNOTATIONS.iter().find(|(name, _)| *name == annotation.name)?;
    Some(json!({ "contents": { "kind": "markdown", "value": format!("**`@file:{name}`**: {doc}.") } }))
}

/// The problems of a script's header as diagnostics.
pub fn diagnostics(text: &str) -> Vec<Value> {
    kargo_ops::script::problems(text)
        .into_iter()
        .map(|problem| json!({ "range": text::range(text, problem.range), "severity": 1, "source": "kargo", "message": problem.message }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offset of the `|` in `text`, which is removed.
    fn at(text: &str) -> (String, usize) {
        let offset = text.find('|').expect("a cursor");
        (text.replacen('|', "", 1), offset)
    }

    fn request_of(text: &str) -> Option<Request> {
        let (text, offset) = at(text);
        request(&text, offset)
    }

    #[test]
    fn completes_annotation_names_after_the_prefix() {
        let request = request_of("@file:Ke|\nprintln(1)\n").expect("request");
        assert_eq!(request, Request::Name { partial: "Ke".into(), range: 6..8 });
        assert_eq!(completion("@file:Ke\n", &request).len(), ANNOTATIONS.len());
    }

    #[test]
    fn offers_the_kargo_annotations_by_name() {
        let (text, offset) = at("@file:|\n");
        let request = request(&text, offset).unwrap();
        let labels: Vec<String> = completion(&text, &request).iter().map(|i| i["label"].as_str().unwrap().to_owned()).collect();
        for name in ["DependsOn", "Repository", "Kotlin", "Jdk", "Framework"] {
            assert!(labels.iter().any(|l| l == name), "{labels:?}");
        }
    }

    #[test]
    fn reads_the_string_argument_being_typed() {
        let request = request_of("@file:Jdk(\"1|\")\n").expect("request");
        assert_eq!(request, Request::Value { name: "Jdk".into(), partial: "1".into(), range: 11..12 });
        let request = request_of("@file:DependsOn([\"a:b:1\", \"c:d|\"])\n").expect("request");
        assert!(matches!(request, Request::Value { ref name, ref partial, .. } if name == "DependsOn" && partial == "c:d"));
    }

    #[test]
    fn answers_jdk_framework_and_repository_values() {
        let (text, offset) = at("@file:Framework(\"|\")\n");
        let request = request(&text, offset).unwrap();
        let labels: Vec<String> = completion(&text, &request).iter().map(|i| i["label"].as_str().unwrap().to_owned()).collect();
        assert!(labels.contains(&"serialization".to_owned()), "{labels:?}");
        let request = Request::Value { name: "Jdk".into(), partial: String::new(), range: 0..0 };
        assert!(completion("", &request).iter().any(|i| i["label"] == "21"));
    }

    #[test]
    fn leaves_positions_outside_annotations_alone() {
        assert_eq!(request_of("val x = \"|\"\n"), None);
        assert_eq!(request_of("@file:Kotlin(\"2.3.0\") |\n"), None);
        let request = request_of("println(1)\n@file:Kotlin(\"2|\n").expect("request");
        assert!(matches!(request, Request::Value { ref name, ref partial, .. } if name == "Kotlin" && partial == "2"));
    }

    #[test]
    fn hovers_annotation_names_only() {
        let (text, offset) = at("@file:Dep|endsOn(\"a:b:1\")\n");
        let shown = hover(&text, offset).expect("hover");
        assert!(shown["contents"]["value"].as_str().unwrap().contains("@file:DependsOn"));
        let (text, offset) = at("@file:DependsOn(\"a:b|:1\")\n");
        assert!(hover(&text, offset).is_none());
    }

    #[test]
    fn reports_header_problems_as_diagnostics() {
        let found = diagnostics("@file:DependsOn(\"a:b\")\n@file:Framework(\"flask\")\nprintln(1)\n");
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0]["range"]["start"]["line"], 0);
        assert_eq!(found[1]["range"]["start"]["line"], 1);
        assert!(found[1]["message"].as_str().unwrap().contains("unknown framework `flask`"));
        assert!(diagnostics("@file:Kotlin(\"2.3.0\")\nprintln(1)\n").is_empty());
    }

    #[test]
    fn tells_kargo_scripts_from_build_scripts_in_projects() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kargo.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n").unwrap();
        let build = dir.path().join("build.kts");
        assert!(!is_kargo_script(&build, "println(1)"), "a project's other scripts are not Kargo scripts");
        assert!(is_kargo_script(&build, "#!/usr/bin/env -S kargo script\nprintln(1)"));
        let loose = dir.path().join("sub").join("fetch.kts");
        assert!(!is_kargo_script(&loose, "println(1)"), "a script under a project is governed by it");
    }
}
