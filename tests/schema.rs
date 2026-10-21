//! Keeps the `kargo.toml` JSON Schema (`website/public/schemas/kargo.json`) in sync with the
//! manifest parser: every key the schema offers must be one `kargo.toml` accepts, and its
//! enums and default versions must match Kargo's own.

use std::path::Path;

use serde_json::{Map, Value};

use kargo_manifest::{
    BENCH_TIME_UNITS, BUILTIN_GENERATORS, BenchMode, GeneratedKind, Hook, InstallerType, Manifest, PackageManager, TargetKind, TaskShell,
    WorkspaceManifest,
};

fn schema() -> Value {
    let text = include_str!("../website/public/schemas/kargo.json");
    serde_json::from_str(text).expect("the schema is valid JSON")
}

/// Follows a `{ "$ref": "#/definitions/<name>" }`.
fn resolve<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    match node.get("$ref").and_then(Value::as_str) {
        Some(reference) => {
            let name = reference.strip_prefix("#/definitions/").expect("only local definitions are referenced");
            &root["definitions"][name]
        }
        None => node,
    }
}

/// A value the schema allows for `node`: its first enum value, constant, example or default,
/// else a value of its type.
fn sample(root: &Value, node: &Value) -> Value {
    let node = resolve(root, node);
    if let Some(v) = node.get("enum").and_then(|e| e.get(0)).or(node.get("const")) {
        return v.clone();
    }
    if let Some(v) = node.get("examples").and_then(|e| e.get(0)).or(node.get("default")) {
        return v.clone();
    }
    if let Some(alternatives) = node.get("anyOf").and_then(Value::as_array) {
        return sample(root, &alternatives[0]);
    }
    let ty = match &node["type"] {
        Value::Array(types) => types[0].as_str().unwrap_or("string"),
        other => other.as_str().unwrap_or("string"),
    };
    match ty {
        "boolean" => Value::Bool(true),
        "integer" => Value::from(1),
        "array" => Value::Array(vec![sample(root, &node["items"])]),
        "object" => Value::Object(Map::new()),
        _ => Value::String("x".to_owned()),
    }
}

/// Every key path the schema describes, with a sample value. A map whose keys the user picks
/// (dependencies, tasks, ...) gets the key `sample`; `[]` stands for an element of an array of tables.
fn leaves(root: &Value, node: &Value, path: Vec<String>, out: &mut Vec<(Vec<String>, Value)>) {
    let node = resolve(root, node);
    let mut nested = false;
    if let Some(props) = node.get("properties").and_then(Value::as_object) {
        nested = true;
        for (key, child) in props {
            let mut p = path.clone();
            p.push(key.clone());
            leaves(root, child, p, out);
        }
    }
    if let Some(map) = node.get("additionalProperties").filter(|v| v.is_object()) {
        nested = true;
        let mut p = path.clone();
        p.push("sample".to_owned());
        leaves(root, map, p, out);
    }
    if let Some(alternatives) = node.get("anyOf").and_then(Value::as_array) {
        for alt in alternatives {
            let alt = resolve(root, alt);
            if alt.get("properties").is_some() || alt.get("additionalProperties").is_some_and(Value::is_object) {
                nested = true;
                leaves(root, alt, path.clone(), out);
            }
        }
    }
    if node["type"] == "array" && resolve(root, &node["items"]).get("properties").is_some() {
        nested = true;
        let mut p = path.clone();
        p.push("[]".to_owned());
        leaves(root, &node["items"], p, out);
    }
    if !nested && !path.is_empty() {
        out.push((path, sample(root, node)));
    }
}

/// `value` as an inline TOML value.
fn toml_value(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let entries: Vec<String> = map.iter().map(|(k, v)| format!("{} = {}", Value::from(k.as_str()), toml_value(v))).collect();
            format!("{{ {} }}", entries.join(", "))
        }
        Value::Array(items) => format!("[{}]", items.iter().map(toml_value).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

/// The schema node at `path`, through `properties`, `additionalProperties` (for `sample`) and
/// the alternatives of `anyOf`.
fn node_at<'a>(root: &'a Value, node: &'a Value, path: &[String]) -> Option<&'a Value> {
    let node = resolve(root, node);
    let Some((first, rest)) = path.split_first() else { return Some(node) };
    let child = if first == "sample" { node.get("additionalProperties") } else { node.get("properties").and_then(|p| p.get(first)) };
    if let Some(found) = child.and_then(|c| node_at(root, c, rest)) {
        return Some(found);
    }
    node.get("anyOf")?.as_array()?.iter().find_map(|alt| node_at(root, alt, path))
}

/// A one-line TOML document setting `path` to `value` (with dotted, quoted keys).
fn toml_line(root: &Value, path: &[String], value: &Value) -> String {
    if let Some(i) = path.iter().position(|s| s == "[]") {
        // An array of tables, such as [[bin]]: one element with the keys it requires (a name,
        // unless the schema says otherwise) and the key.
        let mut element = Map::new();
        let items = node_at(root, root, &path[..i]).map(|array| resolve(root, &array["items"]));
        match items.and_then(|items| items.get("required").and_then(Value::as_array).map(|r| (items, r))) {
            Some((items, required)) => {
                for key in required.iter().filter_map(Value::as_str) {
                    element.insert(key.to_owned(), sample(root, &items["properties"][key]));
                }
            }
            None => {
                element.insert("name".to_owned(), Value::from("tool"));
            }
        }
        let rest = &path[i + 1..];
        let mut inner = value.clone();
        for key in rest.iter().rev() {
            let mut map = Map::new();
            map.insert(key.clone(), inner);
            inner = Value::Object(map);
        }
        if let Value::Object(map) = inner {
            element.extend(map);
        }
        return format!("{} = [{}]", keys(&path[..i]), toml_value(&Value::Object(element)));
    }
    format!("{} = {}", keys(path), toml_value(value))
}

fn keys(path: &[String]) -> String {
    path.iter().map(|k| Value::from(k.as_str()).to_string()).collect::<Vec<_>>().join(".")
}

#[test]
fn every_schema_key_is_accepted_by_the_parser() {
    let root = schema();
    let mut all = Vec::new();
    leaves(&root, &root, Vec::new(), &mut all);
    assert!(all.len() > 100, "the schema should describe every key, found {}", all.len());
    for (path, value) in all {
        let line = toml_line(&root, &path, &value);
        let error = if path[0] == "workspace" {
            let mut text = String::from("workspace.members = [\"app\"]\n");
            if path[1] != "members" {
                text.push_str(&line);
            }
            WorkspaceManifest::parse(&text, Path::new(".")).err()
        } else {
            let mut text = String::new();
            for (key, v) in [("name", "demo"), ("version", "0.1.0")] {
                if path != ["package", key] {
                    text.push_str(&format!("package.{key} = \"{v}\"\n"));
                }
            }
            text.push_str(&line);
            Manifest::parse(&text).err()
        };
        // Other validation errors are fine: only the key itself must be known.
        if let Some(e) = error {
            let message = format!("{e:#}");
            assert!(
                !message.contains("unknown field") && !message.contains("unknown variant"),
                "`{line}` from the schema is rejected: {message}"
            );
        }
    }
}

fn enum_values(node: &Value) -> Vec<&str> {
    node["enum"].as_array().expect("an enum").iter().map(|v| v.as_str().unwrap()).collect()
}

fn property_names(node: &Value) -> Vec<&str> {
    node["properties"].as_object().expect("properties").keys().map(String::as_str).collect()
}

#[test]
fn schema_enums_match_kargo() {
    let root = schema();
    let props = &root["properties"];
    let names = |all: Vec<&'static str>| all;
    assert_eq!(enum_values(&root["definitions"]["hook"]), names(Hook::ALL.iter().map(|h| h.as_str()).collect()));
    assert_eq!(
        enum_values(&root["definitions"]["tasks"]["additionalProperties"]["anyOf"][1]["properties"]["shell"]),
        names(TaskShell::ALL.iter().map(|s| s.as_str()).collect())
    );
    assert_eq!(
        enum_values(&props["target"]["properties"]["wasm"]["properties"]["package-manager"]),
        names(PackageManager::ALL.iter().map(|p| p.as_str()).collect())
    );
    assert_eq!(enum_values(&root["definitions"]["installer-type"]), names(InstallerType::ALL.iter().map(|t| t.as_str()).collect()));
    assert_eq!(enum_values(&props["bench"]["properties"]["mode"]), names(BenchMode::ALL.iter().map(|m| m.as_str()).collect()));
    assert_eq!(enum_values(&props["bench"]["properties"]["time-unit"]), names(BENCH_TIME_UNITS.to_vec()));
    let generate = &props["generate"]["items"]["properties"];
    assert_eq!(enum_values(&generate["kind"]), names(GeneratedKind::ALL.iter().map(|k| k.as_str()).collect()));
    assert_eq!(enum_values(&generate["builtin"]), names(BUILTIN_GENERATORS.to_vec()));
    assert_eq!(enum_values(&generate["shell"]), names(TaskShell::ALL.iter().map(|s| s.as_str()).collect()));
    assert_eq!(enum_values(&generate["targets"]["items"]), names(TargetKind::ALL.iter().map(|t| t.as_str()).collect()));
    let mut targets = property_names(&props["target"]);
    let mut expected: Vec<&str> = TargetKind::ALL.iter().map(|t| t.as_str()).collect();
    targets.sort();
    expected.sort();
    assert_eq!(targets, expected);
    let mut frameworks = property_names(&root["definitions"]["frameworks"]);
    let mut expected = kargo_ops::builtins::FRAMEWORK_NAMES.to_vec();
    frameworks.sort();
    expected.sort();
    assert_eq!(frameworks, expected);
}

#[test]
fn schema_defaults_match_kargo() {
    let root = schema();
    let props = &root["properties"];
    let default = |node: &Value| node["default"].as_str().map(str::to_owned);
    let framework = |name: &str| default(&root["definitions"]["frameworks"]["properties"][name]["anyOf"][1]["properties"]["version"]);
    use kargo_ops::builtins::*;
    let cases = [
        (default(&props["kotlin"]["properties"]["version"]), kargo_manifest::DEFAULT_KOTLIN_VERSION),
        (default(&props["kotlin"]["properties"]["ksp"]), KSP_VERSION),
        (default(&props["doc"]["properties"]["dokka"]), kargo_ops::ops::doc::DOKKA_VERSION),
        (default(&props["fmt"]["properties"]["ktlint"]), kargo_ops::ops::fmt::KTLINT_VERSION),
        (default(&props["lint"]["properties"]["detekt"]), kargo_ops::ops::lint::DETEKT_VERSION),
        (default(&props["target"]["properties"]["android"]["properties"]["agp-version"]), kargo_ops::gradle::AGP_VERSION),
        (framework("serialization"), SERIALIZATION_VERSION),
        (framework("compose"), COMPOSE_VERSION),
        (framework("ktor"), KTOR_VERSION),
        (framework("lombok"), LOMBOK_VERSION),
        (framework("spring-boot"), SPRING_BOOT_VERSION),
        (framework("benchmark"), KOTLINX_BENCHMARK_VERSION),
        (default(&props["generate"]["items"]["properties"]["version"]), kargo_ops::generate::PROTOC_VERSION),
        (default(&props["generate"]["items"]["properties"]["grpc-java-version"]), kargo_ops::generate::GRPC_JAVA_VERSION),
        (default(&props["generate"]["items"]["properties"]["grpc-kotlin-version"]), kargo_ops::generate::GRPC_KOTLIN_VERSION),
    ];
    for (schema_default, kargo) in cases {
        assert_eq!(schema_default.as_deref(), Some(kargo), "a default version in the schema is out of date");
    }
    let coroutines = root["definitions"]["frameworks"]["properties"]["coroutines"]["examples"][1].as_str();
    assert_eq!(coroutines, Some(COROUTINES_VERSION));
    let jvm_target = props["target"]["properties"]["jvm"]["properties"]["jvm-target"]["description"].as_str().unwrap();
    assert!(jvm_target.contains(&format!("Defaults to {}", kargo_manifest::DEFAULT_JVM_TARGET)), "{jvm_target}");
}

#[test]
fn native_frameworks_are_lists_of_apple_frameworks() {
    let root = schema();
    let native = &root["properties"]["target"]["properties"]["native"];
    let nodes = [
        &native["properties"]["frameworks"],
        &native["properties"]["cinterop"]["additionalProperties"]["properties"]["frameworks"],
        &root["definitions"]["nativeLinkOptions"]["properties"]["frameworks"],
        &root["definitions"]["nativeOptions"]["properties"]["frameworks"],
    ];
    for node in nodes {
        let resolved = resolve(&root, node);
        assert_eq!(resolved["type"], "array", "{node}");
        assert_eq!(resolved["items"]["type"], "string", "{node}");
    }
    assert_eq!(resolve(&root, &root["properties"]["frameworks"])["type"], "object");
}
