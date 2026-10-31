//! Records what a build, `check`, `test` or `run` prints, and keeps the output of the last one
//! that failed in `target/.kargo/last-error.log`, which `kargo report` attaches to a bug report.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Where the output of the last failed command is kept, inside a target directory.
pub fn last_error_path(target_dir: &Path) -> PathBuf {
    target_dir.join(".kargo").join("last-error.log")
}

/// The most lines and bytes kept, so a build that fails after printing a lot still leaves a file
/// that can be pasted into an issue.
const MAX_LINES: usize = 4_000;
const MAX_BYTES: usize = 2 << 20;

#[derive(Debug)]
struct Record {
    command: String,
    version: String,
    /// The target directory of the project being built, once it is known.
    target_dir: Option<PathBuf>,
    lines: Vec<String>,
    bytes: usize,
}

static RECORD: Mutex<Option<Record>> = Mutex::new(None);

/// Starts recording the output of `command` (`build`, `check`, `test` or `run`).
pub fn start(command: &str, version: &str) {
    *RECORD.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(Record { command: command.to_owned(), version: version.to_owned(), target_dir: None, lines: Vec::new(), bytes: 0 });
}

/// Records where the output of a failed `build`, `check`, `test` or `run` goes.
pub fn set_target_dir(dir: &Path) {
    if let Some(record) = RECORD.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        record.target_dir = Some(dir.to_path_buf());
    }
}

/// Adds one line to the recording, if one is running.
pub fn line(text: impl AsRef<str>) {
    let mut guard = RECORD.lock().unwrap_or_else(|e| e.into_inner());
    let Some(record) = guard.as_mut() else { return };
    if record.lines.len() >= MAX_LINES || record.bytes >= MAX_BYTES {
        return;
    }
    let mut line = strip_ansi(text.as_ref().trim_end_matches(['\r', '\n']));
    if line.len() > 4_000 {
        let mut cut = 4_000;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
    }
    record.bytes += line.len() + 1;
    record.lines.push(line);
}

/// Removes the terminal escape sequences of colored output, so a report pasted into an issue is
/// plain text.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // CSI: `ESC [ ...` ends with a letter or `@`.
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() || c == '@' {
                        break;
                    }
                }
            }
            // OSC: `ESC ] ...` ends with BEL or `ESC \`.
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Ends the recording: a failure writes `last-error.log`, a success keeps the last one.
pub fn finish(success: bool) {
    let record = RECORD.lock().unwrap_or_else(|e| e.into_inner()).take();
    if success {
        return;
    }
    if let Some(record) = record {
        write(&record);
    }
}

/// Records a panic and writes `last-error.log` at once: the process is about to end.
pub fn panic(message: &str) {
    let mut guard = RECORD.lock().unwrap_or_else(|e| e.into_inner());
    let Some(record) = guard.as_mut() else { return };
    record.lines.push("error: kargo panicked".to_owned());
    record.lines.push(message.to_owned());
    record.bytes += message.len();
    write(record);
}

fn write(record: &Record) {
    let Some(dir) = &record.target_dir else { return };
    let path = last_error_path(dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut text = format!("# `kargo {}` failed at {}\nkargo: {}\n\n", record.command, iso8601(now_seconds()), record.version);
    for line in &record.lines {
        text.push_str(line);
        text.push('\n');
    }
    let _ = std::fs::write(path, text);
}

fn now_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

/// A writer that passes everything to `inner` and records its lines, for output Kargo prints to
/// stdout without going through [`crate::shell::Shell`] (the test report).
#[derive(Debug)]
pub struct Tee<W: Write> {
    inner: W,
    pending: Vec<u8>,
}

impl<W: Write> Tee<W> {
    pub fn new(inner: W) -> Self {
        Self { inner, pending: Vec::new() }
    }
}

impl<W: Write> Write for Tee<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        for &byte in &buf[..written] {
            if byte == b'\n' {
                line(String::from_utf8_lossy(&self.pending));
                self.pending.clear();
            } else if self.pending.len() < 4_000 {
                self.pending.push(byte);
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Drop for Tee<W> {
    fn drop(&mut self) {
        if !self.pending.is_empty() {
            line(String::from_utf8_lossy(&self.pending));
            self.pending.clear();
        }
    }
}

/// A UTC timestamp, `YYYY-MM-DDTHH:MM:SSZ`.
pub fn iso8601(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// The calendar date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recording is one process-wide value, so these tests run one at a time.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn formats_utc_timestamps() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601(1_790_000_000), "2026-09-21T14:13:20Z");
    }

    #[test]
    fn a_failure_writes_the_last_error_log() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        start("build", "0.1.4");
        set_target_dir(dir.path());
        line("   Compiling app v0.1.0");
        line("src/Main.kt:2:16: error: unresolved reference 'x'.");
        finish(false);
        let text = std::fs::read_to_string(last_error_path(dir.path())).unwrap();
        assert!(text.starts_with("# `kargo build` failed at "), "{text}");
        assert!(text.contains("\nkargo: 0.1.4\n"), "{text}");
        assert!(text.contains("src/Main.kt:2:16: error: unresolved reference 'x'."), "{text}");
    }

    #[test]
    fn a_success_keeps_the_earlier_log_and_a_panic_writes_it_at_once() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        start("test", "0.1.4");
        set_target_dir(dir.path());
        line("a test failed");
        finish(false);
        let before = std::fs::read_to_string(last_error_path(dir.path())).unwrap();

        start("test", "0.1.4");
        set_target_dir(dir.path());
        line("all tests pass");
        finish(true);
        assert_eq!(std::fs::read_to_string(last_error_path(dir.path())).unwrap(), before);

        start("run", "0.1.4");
        set_target_dir(dir.path());
        panic("index out of bounds");
        let after = std::fs::read_to_string(last_error_path(dir.path())).unwrap();
        assert!(after.contains("`kargo run` failed"), "{after}");
        assert!(after.contains("index out of bounds"), "{after}");
    }

    #[test]
    fn records_lines_written_through_a_tee_without_escape_sequences() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        start("test", "0.1.4");
        set_target_dir(dir.path());
        let mut tee = Tee::new(Vec::new());
        tee.write_all("\u{1b}[32mPASSED\u{1b}[0m OneTest\nFAILED Two".as_bytes()).unwrap();
        drop(tee);
        finish(false);
        let text = std::fs::read_to_string(last_error_path(dir.path())).unwrap();
        assert!(text.contains("PASSED OneTest"), "{text:?}");
        assert!(text.contains("FAILED Two"), "{text:?}");
        assert!(!text.contains('\u{1b}'), "{text:?}");
    }

    #[test]
    fn a_failure_without_a_project_writes_nothing() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        start("build", "0.1.4");
        line("error: could not find `kargo.toml`");
        finish(false);
        assert!(!dir.path().join(".kargo").exists());
    }
}
