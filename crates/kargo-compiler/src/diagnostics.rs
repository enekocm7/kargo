//! Compiler diagnostics. By default compiler output goes to stderr unchanged, except
//! that the `-Werror` summary the compilers print first comes last; with
//! `--message-format json` it is parsed into one JSON object per diagnostic (file,
//! line, column, severity, message and the target being compiled) on stdout, so editors
//! don't scrape text.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};

use kargo_util::process;
use kargo_util::shell::Shell;

static JSON: AtomicBool = AtomicBool::new(false);

/// Serializes JSON lines from compilations running in parallel.
static STDOUT: Mutex<()> = Mutex::new(());

thread_local! {
    /// The target this thread compiles, for the `target` field of JSON messages.
    static TARGET: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Runs `f` with JSON messages from this thread tagged with `target` (`jvm`, `wasm`, ...);
/// `None` leaves them untagged. Threads a build spawns for one target inherit it through
/// `kargo_ops::ops::parallel_map`.
pub fn with_target<R>(target: Option<&str>, f: impl FnOnce() -> R) -> R {
    let previous = TARGET.with(|t| t.replace(target.map(str::to_owned)));
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TARGET.with(|t| *t.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(previous);
    f()
}

/// The target set by [`with_target`] on this thread.
pub fn current_target() -> Option<String> {
    TARGET.with(|t| t.borrow().clone())
}

/// Switches compiler output to JSON messages on stdout (`--message-format json`).
pub fn set_json(json: bool) {
    JSON.store(json, Ordering::Relaxed);
}

pub fn is_json() -> bool {
    JSON.load(Ordering::Relaxed)
}

/// Prints one JSON message as a line on stdout.
pub fn emit(message: &serde_json::Value) {
    let _guard = STDOUT.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

/// One message from kotlinc, konanc or javac.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// `error`, `warning`, `info`, `exception` or `note`.
    pub severity: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    /// The diagnostic as the compiler printed it, including the source excerpt.
    pub rendered: String,
}

impl Diagnostic {
    /// The `compiler-message` JSON object. Relative paths are resolved against the
    /// current directory, where the compilers report them from, and use the
    /// platform's separators. `target` is the target being compiled on this thread
    /// (see [`with_target`]), or `null`.
    pub fn to_json(&self) -> serde_json::Value {
        let file = self.file.as_ref().map(|f| {
            let path = Path::new(f);
            let path = match std::env::current_dir() {
                Ok(cwd) if path.is_relative() && !path.has_root() => cwd.join(path),
                _ => path.to_path_buf(),
            };
            path.components().collect::<PathBuf>().display().to_string()
        });
        serde_json::json!({
            "reason": "compiler-message",
            "severity": self.severity,
            "message": self.message,
            "file": file,
            "line": self.line,
            "column": self.column,
            "rendered": self.rendered,
            "target": current_target(),
        })
    }
}

const SEVERITIES: [&str; 4] = ["error", "warning", "info", "exception"];

/// Parses the first line of a diagnostic: `path:line:col: severity: message`
/// (kotlinc and konanc), `path:line: severity: message` (javac),
/// `severity: message` (no location) or javac's `Note: message`.
pub fn parse_header(line: &str) -> Option<Diagnostic> {
    let line = line.trim_end_matches('\r');
    let diagnostic = |severity: &str, message: &str, location: Option<(String, Option<u32>, Option<u32>)>| {
        let (file, line_no, column) = match location {
            Some((file, line, column)) => (Some(file), line, column),
            None => (None, None, None),
        };
        Some(Diagnostic {
            severity: severity.to_owned(),
            message: message.trim().to_owned(),
            file,
            line: line_no,
            column,
            rendered: line.to_owned(),
        })
    };
    if let Some(message) = line.strip_prefix("Note: ") {
        return diagnostic("note", message, None);
    }
    for severity in SEVERITIES {
        if let Some(message) = line.strip_prefix(severity).and_then(|r| r.strip_prefix(": ")) {
            return diagnostic(severity, message, None);
        }
    }
    // The earliest `: severity: ` ends the location.
    let (start, severity) = SEVERITIES.iter().filter_map(|s| line.find(&format!(": {s}: ")).map(|i| (i, *s))).min()?;
    let location = parse_location(&line[..start])?;
    diagnostic(severity, &line[start + severity.len() + 4..], Some(location))
}

/// Splits `path:line[:column]`; the path may contain colons (`C:\src\A.kt`).
fn parse_location(location: &str) -> Option<(String, Option<u32>, Option<u32>)> {
    let number = |s: &str| s.parse::<u32>().ok();
    let (rest, last) = location.rsplit_once(':')?;
    let last = number(last)?;
    if let Some((file, line)) = rest.rsplit_once(':').and_then(|(f, l)| Some((f, number(l)?)))
        && !file.is_empty()
    {
        return Some((file.to_owned(), Some(line), Some(last)));
    }
    (!rest.is_empty()).then(|| (rest.to_owned(), Some(last), None))
}

/// javac's closing `1 error` / `3 warnings` line.
fn is_count_line(line: &str) -> bool {
    let mut words = line.split_whitespace();
    matches!(
        (words.next().map(|n| n.parse::<u32>().is_ok()), words.next(), words.next()),
        (Some(true), Some("error" | "errors" | "warning" | "warnings"), None)
    )
}

/// What the Kotlin compilers report as an error under `-Werror` once they have reported warnings.
const WERROR_SUMMARY: &str = "warnings found and -Werror specified";

/// Whether `line` is the `-Werror` summary. The compilers print it ahead of the warnings
/// (now errors) it counts, because their messages without a location sort first.
fn is_werror_summary(line: &str) -> bool {
    line.trim_end_matches('\r').strip_prefix("error: ") == Some(WERROR_SUMMARY)
}

/// Receives compiler output line by line: in human mode it goes to stderr as is,
/// in JSON mode diagnostics become JSON messages and anything else goes to stderr.
///
/// The `-Werror` summary is held back until [`Relay::flush`], so it ends the output like the
/// summary of any other build instead of announcing diagnostics that have not been printed yet.
#[derive(Debug)]
pub struct Relay<W: Write = std::io::Stderr> {
    current: Option<Diagnostic>,
    summary: Option<Diagnostic>,
    /// Where human-readable lines go.
    err: W,
}

impl Relay {
    pub fn new() -> Self {
        Self::to(std::io::stderr())
    }
}

impl Default for Relay {
    fn default() -> Self {
        Self::new()
    }
}

impl<W: Write> Relay<W> {
    fn to(err: W) -> Self {
        Self { current: None, summary: None, err }
    }

    /// Handles one line (without its line terminator).
    pub fn line(&mut self, line: &str) {
        if is_werror_summary(line) {
            self.emit_current();
            self.summary = parse_header(line);
            return;
        }
        if !is_json() {
            let _ = writeln!(self.err, "{line}");
            kargo_util::capture::line(line);
            return;
        }
        if let Some(diagnostic) = parse_header(line) {
            self.emit_current();
            self.current = Some(diagnostic);
        } else if is_count_line(line) {
            self.emit_current();
            let _ = writeln!(self.err, "{line}");
        } else if let Some(current) = &mut self.current {
            current.rendered.push('\n');
            current.rendered.push_str(line.trim_end_matches('\r'));
        } else {
            let _ = writeln!(self.err, "{line}");
        }
    }

    /// Handles a block of complete lines.
    pub fn text(&mut self, text: &str) {
        for line in text.lines() {
            self.line(line);
        }
    }

    /// Emits the diagnostic in progress (JSON mode).
    fn emit_current(&mut self) {
        if let Some(diagnostic) = self.current.take() {
            emit(&diagnostic.to_json());
        }
    }

    /// Emits the diagnostic in progress and the held-back summary; call when the compiler is done.
    pub fn flush(&mut self) {
        self.emit_current();
        if let Some(summary) = self.summary.take() {
            if is_json() {
                emit(&summary.to_json());
            } else {
                let _ = writeln!(self.err, "{}", summary.rendered);
                kargo_util::capture::line(&summary.rendered);
            }
        }
    }
}

impl<W: Write> Drop for Relay<W> {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Runs a compiler process and relays its diagnostics: with `--message-format json`
/// they become JSON messages, otherwise its stderr is passed on line by line.
pub fn status(shell: &Shell, cmd: &mut Command, what: &str) -> Result<ExitStatus> {
    status_observed(shell, cmd, what, &mut |_| {})
}

/// Like [`status`], and also hands every line the tool reports to `observe`, so the caller can
/// explain a failure (a missing header, an undefined symbol) after it.
pub fn status_observed(shell: &Shell, cmd: &mut Command, what: &str, observe: &mut dyn FnMut(&str)) -> Result<ExitStatus> {
    status_filtered(shell, cmd, what, observe, &mut |_| true)
}

/// Like [`status_observed`], but only relays the lines `keep` accepts; the others still reach
/// `observe`, so a caller can explain a failure from them. Used to drop a tool's internal stack
/// trace from what the user sees.
pub fn status_filtered(
    shell: &Shell,
    cmd: &mut Command,
    what: &str,
    observe: &mut dyn FnMut(&str),
    keep: &mut dyn FnMut(&str) -> bool,
) -> Result<ExitStatus> {
    shell.verbose("Running", format!("`{}`", process::describe(cmd)));
    let start_error = |cmd: &Command| format!("failed to start {what} (`{}`)", cmd.get_program().to_string_lossy());
    if is_json() {
        let output = cmd.output().with_context(|| start_error(cmd))?;
        let mut relay = Relay::new();
        for text in [&output.stderr, &output.stdout] {
            let text = String::from_utf8_lossy(text);
            for line in text.lines() {
                observe(line);
                if keep(line) {
                    relay.line(line);
                }
            }
        }
        relay.flush();
        return Ok(output.status);
    }
    // stdout stays inherited; only stderr, where the compilers report, goes through the relay.
    let mut child = cmd.stderr(Stdio::piped()).spawn().with_context(|| start_error(cmd))?;
    let mut relay = Relay::new();
    if let Some(stderr) = child.stderr.take() {
        for line in BufReader::new(stderr).split(b'\n') {
            let Ok(line) = line else { break };
            let line = String::from_utf8_lossy(&line);
            observe(&line);
            if keep(&line) {
                relay.line(&line);
            }
        }
    }
    relay.flush();
    child.wait().with_context(|| start_error(cmd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kotlin_diagnostics() {
        let d = parse_header("src/Main.kt:2:16: error: unresolved reference 'x'.").unwrap();
        assert_eq!(d.severity, "error");
        assert_eq!(d.message, "unresolved reference 'x'.");
        assert_eq!((d.file.as_deref(), d.line, d.column), (Some("src/Main.kt"), Some(2), Some(16)));

        let d = parse_header(r"C:\p\src\Main.kt:10:1: warning: parameter 'a' is never used").unwrap();
        assert_eq!((d.file.as_deref(), d.line, d.column), (Some(r"C:\p\src\Main.kt"), Some(10), Some(1)));
        assert_eq!(d.severity, "warning");

        let d = parse_header("warning: flag -Xfoo is deprecated").unwrap();
        assert_eq!((d.severity.as_str(), d.file, d.line), ("warning", None, None));
        assert_eq!(d.message, "flag -Xfoo is deprecated");
    }

    #[test]
    fn parses_javac_diagnostics() {
        let d = parse_header(r"C:\p\src\Util.java:5: error: ';' expected").unwrap();
        assert_eq!((d.file.as_deref(), d.line, d.column), (Some(r"C:\p\src\Util.java"), Some(5), None));
        assert_eq!(d.message, "';' expected");
        let d = parse_header("/p/src/Util.java:7: warning: [deprecation] old() in Util has been deprecated").unwrap();
        assert_eq!((d.file.as_deref(), d.line, d.severity.as_str()), (Some("/p/src/Util.java"), Some(7), "warning"));
        assert_eq!(parse_header("Note: Some input files use unchecked operations.").unwrap().severity, "note");
        assert!(is_count_line("1 error"));
        assert!(is_count_line("12 warnings"));
        assert!(!is_count_line("1 error here"));
    }

    #[test]
    fn ignores_other_lines() {
        assert!(parse_header("    val x = y").is_none());
        assert!(parse_header("            ^").is_none());
        assert!(parse_header("note: error: x").is_none());
        assert!(parse_header("Main.kt: error: no line number").is_none());
        assert!(parse_header("info: kotlinc-jvm 2.3.0").is_some());
    }

    #[test]
    fn werror_summary_comes_after_the_diagnostics_it_counts() {
        let mut relay = Relay::to(Vec::new());
        relay.text("error: warnings found and -Werror specified\nsrc/A.kt:1:1: error: 'f' is deprecated.\nf()\n^\n");
        let so_far = String::from_utf8(relay.err.clone()).unwrap();
        assert_eq!(
            so_far, "src/A.kt:1:1: error: 'f' is deprecated.\nf()\n^\n",
            "the summary is held back, the rest is printed as it arrives"
        );
        relay.line("src/A.kt:2:1: error: 'g' is deprecated.");
        relay.flush();
        let out = String::from_utf8(std::mem::take(&mut relay.err)).unwrap();
        assert_eq!(
            out,
            "src/A.kt:1:1: error: 'f' is deprecated.\nf()\n^\nsrc/A.kt:2:1: error: 'g' is deprecated.\nerror: warnings found and -Werror specified\n"
        );
        relay.flush();
        assert!(relay.err.is_empty(), "the summary is printed once");
    }

    #[test]
    fn dropping_a_relay_prints_the_held_back_summary() {
        let mut out = Vec::new();
        {
            let mut relay = Relay::to(&mut out);
            relay.line("error: warnings found and -Werror specified\r");
        }
        assert_eq!(String::from_utf8(out).unwrap(), "error: warnings found and -Werror specified\n");
    }

    #[test]
    fn only_the_exact_summary_is_held_back() {
        assert!(is_werror_summary("error: warnings found and -Werror specified"));
        assert!(is_werror_summary("error: warnings found and -Werror specified\r"));
        assert!(!is_werror_summary("warning: warnings found and -Werror specified"));
        assert!(!is_werror_summary("src/A.kt:1:1: error: warnings found and -Werror specified"));
        assert!(!is_werror_summary("error: warnings found and -Werror specified, really"));
        let mut relay = Relay::to(Vec::new());
        relay.text("warning: unused\nerror: something else\n");
        assert_eq!(String::from_utf8(relay.err.clone()).unwrap(), "warning: unused\nerror: something else\n");
    }

    #[test]
    fn json_message() {
        let d = parse_header("/abs/Main.kt:3:5: error: boom").unwrap();
        let json = d.to_json();
        assert_eq!(json["reason"], "compiler-message");
        assert_eq!(Path::new(json["file"].as_str().unwrap()), Path::new("/abs/Main.kt"));
        assert_eq!(json["line"], 3);
        assert_eq!(json["column"], 5);
        assert_eq!(json["message"], "boom");
        let relative = parse_header("src/Main.kt:1:1: error: x").unwrap().to_json();
        assert!(Path::new(relative["file"].as_str().unwrap()).is_absolute());
        assert!(parse_header("error: x").unwrap().to_json()["file"].is_null());
        assert!(d.to_json()["target"].is_null());
    }

    #[test]
    fn json_message_names_the_target() {
        let d = parse_header("/abs/Main.kt:3:5: error: boom").unwrap();
        let json = with_target(Some("wasm"), || {
            assert_eq!(with_target(Some("native"), || d.to_json())["target"], "native");
            d.to_json()
        });
        assert_eq!(json["target"], "wasm", "the outer target is restored");
        assert!(d.to_json()["target"].is_null());
    }
}
