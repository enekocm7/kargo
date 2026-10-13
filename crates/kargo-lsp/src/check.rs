//! Check on save: `kargo check` in the background when a Kotlin or Java file is saved, its
//! compiler messages turned into diagnostics for the editor.
//!
//! The Kotlin language servers analyze the JVM target with the compiler plugins and flags they
//! know about, so errors only the real build finds (Wasm and Native sources, compiler plugins,
//! `-Werror`) would otherwise not show until `kargo build`. The check runs
//! `kargo check --message-format json` for the package that contains the saved file, and
//! [`crate::server`] merges its diagnostics with the Kotlin server's, dropping the ones that
//! server already reports.
//!
//! It is opt-in through the editor's `initializationOptions`: `{ "checkOnSave": true }`, and
//! optionally `"checkTargets": ["wasm", "native"]` to check only some of the enabled targets.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::text;

/// What `initializationOptions` asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// Whether to check on save (`checkOnSave`).
    pub enabled: bool,
    /// The targets to check (`checkTargets`); empty: every enabled target.
    pub targets: Vec<String>,
}

impl Settings {
    /// Reads `checkOnSave` and `checkTargets` from the editor's `initializationOptions`.
    pub fn from_options(options: &Value) -> Self {
        let targets = options["checkTargets"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
        Self { enabled: options["checkOnSave"] == true, targets }
    }
}

/// Whether saving the file at `uri` checks its package: Kotlin and Java sources.
pub fn is_checked_source(uri: &str) -> bool {
    let name = text::file_name(uri);
    name.ends_with(".kt") || name.ends_with(".java")
}

/// The `kargo.toml` nearest above `file`: its package, or every member of a workspace root.
pub fn manifest_for(file: &Path) -> Option<PathBuf> {
    file.ancestors().skip(1).map(|dir| dir.join(kargo_manifest::MANIFEST_NAME)).find(|m| m.is_file())
}

/// One `kargo check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The `kargo.toml` to check.
    pub manifest: PathBuf,
    /// `--target`s; empty: every enabled target.
    pub targets: Vec<String>,
    pub offline: bool,
}

impl Run {
    /// The arguments after `kargo`.
    pub fn args(&self) -> Vec<String> {
        let mut args: Vec<String> =
            ["check", "--tests", "--keep-going", "--message-format", "json", "--manifest-path"].into_iter().map(str::to_owned).collect();
        args.push(self.manifest.display().to_string());
        for target in &self.targets {
            args.extend(["--target".to_owned(), target.clone()]);
        }
        if self.offline {
            args.push("--offline".to_owned());
        }
        args
    }
}

/// What a check printed.
#[derive(Debug, Clone, Default)]
pub struct Output {
    /// Its JSON messages, one per line of stdout.
    pub messages: Vec<Value>,
    /// What it printed on stderr: status lines, and why it failed before compiling.
    pub stderr: String,
}

impl Output {
    /// Whether it got as far as compiling: it ends with `build-finished`.
    pub fn finished(&self) -> bool {
        self.messages.iter().any(|m| m["reason"] == "build-finished")
    }

    /// Whether it failed without a compiler error to show for it, such as a dependency that
    /// could not be resolved. Its diagnostics then say nothing about the sources.
    pub fn failed_elsewhere(&self) -> bool {
        let failed = self.messages.iter().any(|m| m["reason"] == "build-finished" && m["success"] == false);
        !self.finished()
            || (failed && !self.messages.iter().any(|m| m["reason"] == "compiler-message" && severity(m).is_some_and(|s| s == 1)))
    }
}

/// Runs a check; stops early when the flag is set (a newer save came in).
pub type Runner = Arc<dyn Fn(&Run, &AtomicBool) -> Result<Output> + Send + Sync>;

/// The runner that starts `kargo` itself.
pub fn kargo_runner() -> Runner {
    Arc::new(|run, cancel| {
        let exe = std::env::current_exe().context("failed to find the kargo executable")?;
        run_command(&exe, run, cancel)
    })
}

/// Runs `exe` with the arguments of `run` in the directory of its `kargo.toml`.
pub fn run_command(exe: &Path, run: &Run, cancel: &AtomicBool) -> Result<Output> {
    let mut cmd = Command::new(exe);
    cmd.args(run.args()).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(dir) = run.manifest.parent() {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().with_context(|| format!("failed to run {}", exe.display()))?;
    let reader = |mut stream: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stream.read_to_end(&mut out);
            String::from_utf8_lossy(&out).into_owned()
        })
    };
    let stdout = reader(Box::new(child.stdout.take().context("no stdout")?));
    let stderr = reader(Box::new(child.stderr.take().context("no stderr")?));
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let stdout = stdout.join().unwrap_or_default();
    let messages = stdout.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
    Ok(Output { messages, stderr: stderr.join().unwrap_or_default() })
}

/// The LSP severity of a `compiler-message`: errors and warnings; `info` and javac's notes are
/// left out.
fn severity(message: &Value) -> Option<u8> {
    match message["severity"].as_str()? {
        "error" | "exception" => Some(1),
        "warning" => Some(2),
        _ => None,
    }
}

/// The key a URI is compared by: editors spell the same file differently (`c%3A` and `C:` on
/// Windows).
pub fn uri_key(uri: &str) -> String {
    match text::uri_to_path(uri) {
        Some(path) => {
            let path = path.display().to_string().replace('\\', "/");
            if cfg!(windows) { path.to_lowercase() } else { path }
        }
        None => uri.to_owned(),
    }
}

/// A diagnostic of a check: URI, range (as JSON), severity and message.
type Key = (String, String, u8, String);

/// The diagnostics of a check, by [`uri_key`]: each with the URI to publish it under. Messages
/// without a file (such as the `-Werror` summary) go on `manifest`. A message several targets
/// report alike is one diagnostic whose `code` names them all.
pub fn diagnostics(messages: &[Value], manifest: &Path) -> HashMap<String, (String, Vec<Value>)> {
    // A diagnostic → where it was first reported and the targets that report it.
    let mut found: BTreeMap<Key, (usize, Vec<String>)> = BTreeMap::new();
    let mut texts: HashMap<PathBuf, Option<String>> = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        if message["reason"] != "compiler-message" {
            continue;
        }
        let Some(severity) = severity(message) else { continue };
        let file = message["file"].as_str().map(PathBuf::from);
        let line = message["line"].as_u64().unwrap_or(1).max(1) as usize;
        let range = match &file {
            Some(file) => {
                let source = texts.entry(file.clone()).or_insert_with(|| std::fs::read_to_string(file).ok());
                source_range(source.as_deref(), line - 1, message["column"].as_u64().map(|c| c.max(1) as usize - 1))
            }
            None => json!({ "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }),
        };
        let uri = text::path_to_uri(file.as_deref().unwrap_or(manifest));
        let text = message["message"].as_str().unwrap_or_default().to_owned();
        let entry = found.entry((uri, range.to_string(), severity, text)).or_insert((index, Vec::new()));
        if let Some(target) = message["target"].as_str()
            && !entry.1.iter().any(|t| t == target)
        {
            entry.1.push(target.to_owned());
        }
    }
    let mut ordered: Vec<_> = found.into_iter().collect();
    ordered.sort_by_key(|(_, (index, _))| *index);
    let mut out: HashMap<String, (String, Vec<Value>)> = HashMap::new();
    for ((uri, range, severity, message), (_, mut targets)) in ordered {
        targets.sort();
        let mut diagnostic = json!({
            "range": serde_json::from_str::<Value>(&range).unwrap_or(Value::Null),
            "severity": severity,
            "source": "kargo",
            "message": message,
        });
        if !targets.is_empty() {
            diagnostic["code"] = json!(targets.join(", "));
        }
        out.entry(uri_key(&uri)).or_insert_with(|| (uri, Vec::new())).1.push(diagnostic);
    }
    out
}

/// The range of a compiler message at zero-based `line` and `column` (in characters): the word
/// there, or the line without its indentation when there is no column.
fn source_range(source: Option<&str>, line: usize, column: Option<usize>) -> Value {
    let Some(source) = source else {
        let character = column.unwrap_or(0);
        let position = json!({ "line": line, "character": character });
        return json!({ "start": position, "end": { "line": line, "character": character + 1 } });
    };
    let line_start = text::offset(source, &json!({ "line": line, "character": 0 }));
    let line_end = source[line_start..].find('\n').map_or(source.len(), |i| line_start + i);
    let line_text = source[line_start..line_end].trim_end_matches('\r');
    let span = match column {
        Some(column) => {
            let start = line_text.char_indices().nth(column).map_or(line_text.len(), |(i, _)| i);
            let word: usize = line_text[start..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum();
            let len = if word > 0 { word } else { line_text[start..].chars().next().map_or(0, char::len_utf8) };
            start..start + len
        }
        None => {
            let indent = line_text.len() - line_text.trim_start().len();
            indent..line_text.len()
        }
    };
    text::range(source, line_start + span.start..line_start + span.end)
}

/// Whether the Kotlin server already reports `ours`: a diagnostic on the same line with the
/// same message.
pub fn is_reported(ours: &Value, theirs: &[Value]) -> bool {
    let normalize = |m: &Value| m.as_str().unwrap_or_default().trim().trim_end_matches('.').to_lowercase();
    let line = &ours["range"]["start"]["line"];
    theirs.iter().any(|d| &d["range"]["start"]["line"] == line && normalize(&d["message"]) == normalize(&ours["message"]))
}

/// `theirs` followed by those of `ours` the Kotlin server doesn't report.
pub fn merge(theirs: &[Value], ours: &[Value]) -> Vec<Value> {
    let mut out = theirs.to_vec();
    out.extend(ours.iter().filter(|d| !is_reported(d, theirs)).cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(file: &Path, line: u32, column: u32, target: &str, text: &str) -> Value {
        json!({
            "reason": "compiler-message", "severity": "error", "message": text,
            "file": file.display().to_string(), "line": line, "column": column, "target": target,
        })
    }

    #[test]
    fn reads_settings() {
        assert_eq!(Settings::from_options(&Value::Null), Settings::default());
        let settings = Settings::from_options(&json!({ "checkOnSave": true, "checkTargets": ["wasm", "native"] }));
        assert!(settings.enabled);
        assert_eq!(settings.targets, ["wasm", "native"]);
        assert!(is_checked_source("file:///p/src/Main.kt") && is_checked_source("file:///p/src/A.java"));
        assert!(!is_checked_source("file:///p/kargo.toml") && !is_checked_source("file:///p/build.gradle.kts"));
    }

    #[test]
    fn builds_the_command_line() {
        let run = Run { manifest: PathBuf::from("kargo.toml"), targets: vec!["wasm".into()], offline: true };
        assert_eq!(
            run.args(),
            [
                "check",
                "--tests",
                "--keep-going",
                "--message-format",
                "json",
                "--manifest-path",
                "kargo.toml",
                "--target",
                "wasm",
                "--offline"
            ]
        );
    }

    #[test]
    fn turns_compiler_messages_into_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let common = dir.path().join("src/Main.kt");
        std::fs::create_dir_all(common.parent().unwrap()).unwrap();
        std::fs::write(&common, "fun main() {\n    val x: Int = \"no\"\n    undefined()\n}\n").unwrap();
        let manifest = dir.path().join("kargo.toml");
        let messages = [
            message(&common, 2, 18, "wasm", "type mismatch"),
            message(&common, 2, 18, "native", "type mismatch"),
            message(&common, 3, 5, "native", "unresolved reference 'undefined'."),
            json!({ "reason": "compiler-message", "severity": "info", "message": "kotlinc", "file": null, "target": "wasm" }),
            json!({ "reason": "compiler-message", "severity": "error", "message": "warnings found and -Werror specified", "file": null, "target": "jvm" }),
            json!({ "reason": "build-finished", "success": false }),
        ];
        let found = diagnostics(&messages, &manifest);
        let (uri, list) = &found[&uri_key(&text::path_to_uri(&common))];
        assert_eq!(uri, &text::path_to_uri(&common));
        assert_eq!(list.len(), 2, "{list:?}");
        assert_eq!(list[0]["code"], "native, wasm", "one diagnostic for both targets");
        assert_eq!(list[0]["source"], "kargo");
        assert_eq!(list[0]["range"], json!({ "start": { "line": 1, "character": 17 }, "end": { "line": 1, "character": 18 } }));
        assert_eq!(list[1]["range"], json!({ "start": { "line": 2, "character": 4 }, "end": { "line": 2, "character": 13 } }));
        let (_, on_manifest) = &found[&uri_key(&text::path_to_uri(&manifest))];
        assert_eq!(on_manifest[0]["message"], "warnings found and -Werror specified");
        assert_eq!(on_manifest[0]["code"], "jvm");
    }

    #[test]
    fn tells_a_failed_check_from_compiler_errors() {
        let output = |messages: Vec<Value>| Output { messages, stderr: String::new() };
        assert!(output(vec![]).failed_elsewhere(), "kargo stopped before compiling");
        assert!(output(vec![json!({ "reason": "build-finished", "success": false })]).failed_elsewhere());
        assert!(!output(vec![json!({ "reason": "build-finished", "success": true })]).failed_elsewhere());
        let error = json!({ "reason": "compiler-message", "severity": "error", "message": "x" });
        assert!(!output(vec![error, json!({ "reason": "build-finished", "success": false })]).failed_elsewhere());
    }

    #[test]
    fn leaves_out_what_the_kotlin_server_reports() {
        let at = |line: u32, message: &str| json!({ "range": { "start": { "line": line, "character": 0 } }, "message": message });
        let theirs = [at(1, "Unresolved reference 'x'.")];
        let ours = [at(1, "unresolved reference 'x'"), at(1, "type mismatch"), at(2, "Unresolved reference 'x'.")];
        let merged = merge(&theirs, &ours);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[1]["message"], "type mismatch");
        assert_eq!(merged[2]["range"]["start"]["line"], 2);
    }

    #[test]
    fn compares_uris_by_path() {
        if cfg!(windows) {
            assert_eq!(uri_key("file:///c%3A/p/Main.kt"), uri_key(&text::path_to_uri(Path::new("C:\\p\\Main.kt"))));
        }
        assert_eq!(uri_key("file:///p/Main.kt"), uri_key("file:///p/Main.kt"));
        assert_ne!(uri_key("file:///p/Main.kt"), uri_key("file:///p/Other.kt"));
    }
}
