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

    /// Whether there is a person to ask: stdin and stderr are both terminals.
    pub fn is_interactive(&self) -> bool {
        std::io::IsTerminal::is_terminal(&std::io::stdin()) && std::io::IsTerminal::is_terminal(&std::io::stderr())
    }

    /// Shows `items` under `title` as a list the user moves through with the arrow keys (or `j`/`k`) and picks from
    /// with Enter. Returns the index of the item picked, or `None` when the user cancels (Esc, `q` or Ctrl-C).
    /// Fails when there is no terminal to ask on; check [`Shell::is_interactive`] first.
    pub fn select(&self, title: impl Display, items: &[String]) -> std::io::Result<Option<usize>> {
        use console::{Key, Term};
        let term = Term::stderr();
        if items.is_empty() || !self.is_interactive() {
            return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no terminal to ask on"));
        }
        let hint = "(arrow keys to move, Enter to select, Esc to cancel)";
        term.write_line(&format!("{title} {hint}"))?;
        term.hide_cursor()?;
        let mut at = 0usize;
        let draw = |at: usize, first: bool| -> std::io::Result<()> {
            if !first {
                term.clear_last_lines(items.len())?;
            }
            for (i, item) in items.iter().enumerate() {
                let (mark, style) = if i == at { ('>', NOTE) } else { (' ', Style::new()) };
                term.write_line(&format!("{style}{mark} {item}{style:#}"))?;
            }
            Ok(())
        };
        draw(at, true)?;
        let picked = loop {
            match term.read_key() {
                Ok(Key::ArrowUp | Key::Char('k')) => at = at.checked_sub(1).unwrap_or(items.len() - 1),
                Ok(Key::ArrowDown | Key::Char('j') | Key::Tab) => at = (at + 1) % items.len(),
                Ok(Key::Home) => at = 0,
                Ok(Key::End) => at = items.len() - 1,
                Ok(Key::Enter) => break Some(at),
                Ok(Key::Escape | Key::Char('q') | Key::CtrlC) | Err(_) => break None,
                Ok(Key::Char(c)) if c.is_ascii_digit() && c != '0' && (c as usize - '0' as usize) <= items.len() => {
                    break Some(c as usize - '1' as usize);
                }
                Ok(_) => continue,
            }
            draw(at, false)?;
        };
        term.clear_last_lines(items.len() + 1)?;
        term.show_cursor()?;
        Ok(picked)
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
