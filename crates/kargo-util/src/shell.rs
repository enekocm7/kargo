//! Cargo-style terminal output: right-aligned, colored status verbs.

use std::fmt::Display;
use std::io::Write;

use anstyle::{AnsiColor, Style};

/// How chatty the shell should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Verbosity {
    Quiet,
    #[default]
    Normal,
    Verbose,
}

/// Writes status messages to stderr, leaving stdout for program output.
#[derive(Debug, Clone, Default)]
pub struct Shell {
    verbosity: Verbosity,
}

const STATUS: Style = AnsiColor::Green.on_default().bold();
const WARN: Style = AnsiColor::Yellow.on_default().bold();
const ERROR: Style = AnsiColor::Red.on_default().bold();
const NOTE: Style = AnsiColor::Cyan.on_default().bold();

impl Shell {
    pub fn new(verbosity: Verbosity) -> Self {
        Self { verbosity }
    }

    pub fn verbosity(&self) -> Verbosity {
        self.verbosity
    }

    pub fn is_verbose(&self) -> bool {
        self.verbosity == Verbosity::Verbose
    }

    /// Prints `   Compiling foo v0.1.0` style lines.
    pub fn status(&self, verb: impl Display, message: impl Display) {
        if self.verbosity >= Verbosity::Normal {
            self.print(STATUS, verb, message);
        }
    }

    /// Like [`Shell::status`] but only shown with `--verbose`.
    pub fn verbose(&self, verb: impl Display, message: impl Display) {
        if self.verbosity >= Verbosity::Verbose {
            self.print(NOTE, verb, message);
        }
    }

    pub fn warn(&self, message: impl Display) {
        if self.verbosity >= Verbosity::Normal {
            self.print_left(WARN, "warning", message);
        }
    }

    pub fn note(&self, message: impl Display) {
        if self.verbosity >= Verbosity::Normal {
            self.print_left(NOTE, "note", message);
        }
    }

    /// Errors are always printed, even in quiet mode.
    pub fn error(&self, message: impl Display) {
        self.print_left(ERROR, "error", message);
    }

    /// Prints a block of text, such as license terms, on stderr.
    pub fn text(&self, text: impl Display) {
        if self.verbosity >= Verbosity::Normal {
            let mut err = anstream::stderr().lock();
            let _ = writeln!(err, "{text}");
        }
    }

    /// Shows `text` and asks `question` on the terminal, for a yes or no. `None` when stdin or stderr is not a
    /// terminal, so there is nobody to answer.
    pub fn confirm(&self, text: impl Display, question: impl Display) -> Option<bool> {
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) || !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            return None;
        }
        {
            let mut err = anstream::stderr().lock();
            let _ = write!(err, "{text}\n\n{question} [y/N] ");
            let _ = err.flush();
        }
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).ok()?;
        Some(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
    }

    /// Clears the terminal before a watched run (`--clear`), when stderr is a terminal.
    pub fn clear(&self) {
        if self.verbosity >= Verbosity::Normal && std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            let mut err = anstream::stderr().lock();
            let _ = write!(err, "\x1b[2J\x1b[1;1H");
        }
    }

    fn print(&self, style: Style, verb: impl Display, message: impl Display) {
        let mut err = anstream::stderr().lock();
        let _ = writeln!(err, "{style}{verb:>12}{style:#} {message}");
    }

    fn print_left(&self, style: Style, label: &str, message: impl Display) {
        let mut err = anstream::stderr().lock();
        let _ = writeln!(err, "{style}{label}{style:#}: {message}");
    }
}
