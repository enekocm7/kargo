//! `/kargo-metadata`: the project as `kargo metadata --no-deps` describes it, in Markdown for
//! the assistant.

use std::fmt::Write;

use zed_extension_api::serde_json::Value;

/// Describes the project in `metadata`. `read` returns a file of the worktree by its path
/// relative to the worktree root, for each package's `kargo.toml`.
pub fn describe(metadata: &Value, read: impl Fn(&str) -> Option<String>) -> String {
    let root = str_of(metadata, "root");
    let relative = |path: &str| -> String {
        let rel = path.strip_prefix(root).unwrap_or(path).trim_start_matches(['/', '\\']).replace('\\', "/");
        if rel.is_empty() { ".".to_owned() } else { rel }
    };
    let mut out = String::new();
    let _ = writeln!(out, "# Kargo project\n");
    let _ = writeln!(
        out,
        "Built with Kargo (Cargo for Kotlin): `kargo build`, `kargo run`, `kargo test`, `kargo add <group:artifact>`. Kotlin {}, JDK `{}`.",
        str_of(&metadata["toolchain"], "kotlin"),
        str_of(&metadata["toolchain"]["jdk"], "request"),
    );
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let member = package["member"] == true;
        let _ = writeln!(
            out,
            "\n## Package `{}` {}{}\n",
            str_of(package, "name"),
            str_of(package, "version"),
            if member { "" } else { " (path dependency)" }
        );
        let _ = writeln!(out, "- Directory: `{}`", relative(str_of(package, "root")));
        for target in package["targets"].as_array().into_iter().flatten() {
            let roots = |key: &str| -> String {
                let dirs: Vec<String> =
                    target[key].as_array().into_iter().flatten().filter_map(Value::as_str).map(|p| format!("`{}`", relative(p))).collect();
                if dirs.is_empty() { "none".to_owned() } else { dirs.join(", ") }
            };
            let mut line =
                format!("- Target `{}`: sources {}, tests {}", str_of(target, "kind"), roots("source-roots"), roots("test-roots"));
            if let Some(main) = target["main-class"].as_str() {
                let _ = write!(line, ", main class `{main}`");
            }
            let _ = writeln!(out, "{line}");
        }
        let names = |key: &str| -> Vec<String> {
            package[key].as_array().into_iter().flatten().map(|b| format!("`{}`", str_of(b, "name"))).collect()
        };
        for (key, title) in [("bins", "Programs"), ("examples", "Examples"), ("tasks", "Tasks")] {
            let list = names(key);
            if !list.is_empty() {
                let _ = writeln!(out, "- {title}: {}", list.join(", "));
            }
        }
        let manifest = format!("{}/kargo.toml", relative(str_of(package, "root")));
        let manifest = manifest.trim_start_matches("./");
        if let Some(text) = read(manifest) {
            let _ = writeln!(out, "\n`{manifest}` (dependencies, frameworks and settings):\n\n```toml\n{}\n```", text.trim_end());
        }
    }
    out
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zed_extension_api::serde_json::json;

    #[test]
    fn describes_packages_targets_and_manifests() {
        let metadata = json!({
            "root": "/p",
            "toolchain": { "kotlin": "2.4.21", "jdk": { "request": "system" } },
            "packages": [{
                "name": "app", "version": "0.1.0", "member": true, "root": "/p",
                "bins": [{ "name": "tool" }], "tasks": [{ "name": "lint" }],
                "targets": [{ "kind": "jvm", "source-roots": ["/p/src", "/p/src-jvm"], "test-roots": ["/p/tests"], "main-class": "a.MainKt" }]
            }, {
                "name": "lib", "version": "1.0.0", "member": false, "root": "/p/lib", "targets": []
            }]
        });
        let text = describe(&metadata, |path| (path == "kargo.toml").then(|| "[package]\nname = \"app\"\n".to_owned()));
        assert!(text.contains("Kotlin 2.4.21, JDK `system`"), "{text}");
        assert!(text.contains("- Target `jvm`: sources `src`, `src-jvm`, tests `tests`, main class `a.MainKt`"), "{text}");
        assert!(text.contains("- Programs: `tool`"), "{text}");
        assert!(text.contains("```toml\n[package]\nname = \"app\"\n```"), "{text}");
        assert!(text.contains("## Package `lib` 1.0.0 (path dependency)"), "{text}");
        assert!(text.contains("- Directory: `lib`"), "{text}");
    }
}
