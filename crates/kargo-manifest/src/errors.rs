//! Manifest errors that help: "did you mean" for an unknown key and a source excerpt with a caret for the line a
//! message is about, as Cargo prints them.

use anyhow::Error;
use kargo_util::help;

/// Makes `err`, from parsing or validating the manifest in `text`, more helpful: an unknown key or value gets the
/// closest valid one and the table it was written in, and a message about a key gets the line it is on.
pub(crate) fn explain(text: &str, err: Error) -> Error {
    let unknown = err.chain().find_map(|cause| Unknown::parse(&cause.to_string()));
    let has_excerpt = err.chain().any(|cause| cause.downcast_ref::<toml::de::Error>().is_some_and(|e| e.span().is_some()));
    let full = format!("{err:#}").trim_end().to_owned();
    let span = err.chain().find_map(|cause| cause.downcast_ref::<toml::de::Error>()?.span());

    let message = if has_excerpt {
        full.clone()
    } else {
        match locate(text, &full, unknown.as_ref().map(|u| u.name.as_str())) {
            Some(spot) => format!("error at line {}, column {}\n{}\n{full}", spot.line + 1, spot.column + 1, excerpt(text, &spot)),
            None => full.clone(),
        }
    };
    let Some(unknown) = unknown else {
        // Advice an error already carries survives the rewrite.
        return match help::advice(&err).and_then(|a| Some((a.help()?.to_owned(), a.suggestions().to_vec()))) {
            Some((help_line, suggestions)) => help::helpful(message, help_line, suggestions),
            None => anyhow::anyhow!("{message}"),
        };
    };

    // With a span the table is known from the text (none for a top-level table); the message is the fallback for validation errors.
    let table = match &span {
        Some(span) => table_of(text, span.start),
        None => table_from_message(&full),
    };
    let place = table.as_deref().map(|t| format!(" in `[{t}]`")).unwrap_or_default();
    let expected: Vec<&str> = unknown.expected.iter().map(String::as_str).collect();
    let suggestions: Vec<String> = help::nearest(&unknown.name, &expected, 3).into_iter().map(str::to_owned).collect();
    let help_line = match suggestions.first() {
        Some(close) => format!("{} `{}`{place} is not known; did you mean `{close}`?", unknown.kind, unknown.name),
        None => format!("{} `{}`{place} is not known", unknown.kind, unknown.name),
    };
    help::helpful(message, help_line, suggestions)
}

/// An `unknown field` or `unknown variant` message of serde.
struct Unknown {
    /// `key` or `value`.
    kind: &'static str,
    name: String,
    expected: Vec<String>,
}

impl Unknown {
    fn parse(message: &str) -> Option<Self> {
        let (kind, rest) = if let Some(rest) = message.find("unknown field `").map(|i| &message[i + "unknown field ".len()..]) {
            ("key", rest)
        } else {
            ("value", message.find("unknown variant `").map(|i| &message[i + "unknown variant ".len()..])?)
        };
        let mut names = backticked(rest.lines().next()?);
        if names.is_empty() {
            return None;
        }
        let name = names.remove(0);
        Some(Unknown { kind, name, expected: names })
    }
}

/// The backticked words of `text`.
fn backticked(text: &str) -> Vec<String> {
    text.split('`').skip(1).step_by(2).map(str::to_owned).collect()
}

/// Where in the text a message points.
struct Spot {
    line: usize,
    column: usize,
    width: usize,
}

/// The line `message` is about: the unknown key, else the first key it names (`` `jvm-target` ``, or `[target.jvm]
/// jvm-target`) inside the table it names. The column and width cover the offending value when the line has it.
fn locate(text: &str, message: &str, unknown: Option<&str>) -> Option<Spot> {
    let sections = sections_in(message);
    let ticks = backticked(message);
    let mut keys: Vec<String> = unknown.map(str::to_owned).into_iter().collect();
    keys.extend(sections.iter().filter_map(|s| word_after_section(message, s)));
    keys.extend(ticks.iter().filter(|t| is_key(t)).cloned());
    for key in &keys {
        let found = sections.iter().find_map(|s| find_key(text, Some(s), key)).or_else(|| find_key(text, None, key));
        if let Some(mut spot) = found {
            // The value the message quotes, when it is on the same line after the key.
            if unknown != Some(key.as_str()) {
                let line = text.lines().nth(spot.line).unwrap_or_default();
                let from = spot.column + spot.width;
                let value = ticks
                    .iter()
                    .filter(|t| *t != key && t.len() > 1)
                    .find_map(|t| Some((line.get(from..)?.find(t.as_str())? + from, t.len())));
                if let Some((column, width)) = value {
                    spot.column = column;
                    spot.width = width;
                }
            }
            return Some(spot);
        }
    }
    None
}

/// Whether `word` could be a key or a table's name: letters, digits, `-`, `_`, `.` and `:` (a dependency).
fn is_key(word: &str) -> bool {
    !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// The `[a.b]` tables named in `message`.
fn sections_in(message: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = message;
    while let Some(open) = rest.find('[') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find(']') else { break };
        let name = &rest[..close];
        if is_key(name) && !found.iter().any(|f| f == name) {
            found.push(name.to_owned());
        }
        rest = &rest[close..];
    }
    found
}

/// The word that follows `[section]` in `message` (the key a message such as ``invalid [target.jvm] jvm-target `x` `` is about).
fn word_after_section(message: &str, section: &str) -> Option<String> {
    let after = message.split_once(&format!("[{section}]"))?.1.trim_start_matches([' ', '`']);
    let word: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).collect();
    (!word.is_empty()).then_some(word)
}

/// The header of the table a line starts, as its dotted name; `[[bin]]` is `bin`.
fn header(line: &str) -> Option<&str> {
    let line = line.trim();
    let inner =
        line.strip_prefix("[[").and_then(|l| l.split_once("]]")).or_else(|| line.strip_prefix('[').and_then(|l| l.split_once(']')))?;
    Some(inner.0.trim())
}

/// The line and column of the definition of `key`, in the table `section` when given.
fn find_key(text: &str, section: Option<&str>, key: &str) -> Option<Spot> {
    let mut current = "";
    for (line_index, line) in text.lines().enumerate() {
        if let Some(name) = header(line) {
            current = name;
            continue;
        }
        if section.is_some_and(|s| s != current) {
            continue;
        }
        let code = line.split('#').next().unwrap_or(line);
        let mut from = 0;
        while let Some(found) = code[from..].find(key) {
            let start = from + found;
            let end = start + key.len();
            let before = code[..start].trim_end_matches('"');
            let after = code[end..].trim_start_matches('"').trim_start();
            let word_before = before.chars().next_back().is_none_or(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')));
            if word_before && after.starts_with('=') {
                return Some(Spot { line: line_index, column: start, width: key.len() });
            }
            from = end;
        }
    }
    None
}

/// The table written at (or around) byte `at`: for a key, the table whose header precedes it; for a table header, the table it is
/// declared in (`target` for `[target.jvmm]`).
fn table_of(text: &str, at: usize) -> Option<String> {
    let line_start = text[..at.min(text.len())].rfind('\n').map_or(0, |i| i + 1);
    let lines: Vec<&str> = text[..line_start].lines().collect();
    let own = text[line_start..].lines().next().unwrap_or_default();
    if let Some(name) = header(own) {
        let parent = name.rsplit_once('.').map(|(parent, _)| parent.to_owned());
        return parent;
    }
    lines.iter().rev().find_map(|line| header(line)).map(str::to_owned)
}

/// The table a message names, such as ``invalid dependency `x` in [dependencies]``.
fn table_from_message(message: &str) -> Option<String> {
    sections_in(message).into_iter().next()
}

/// A source excerpt of the line of `spot`, with carets under the key (or value).
fn excerpt(text: &str, spot: &Spot) -> String {
    let line = text.lines().nth(spot.line).unwrap_or_default();
    let number = (spot.line + 1).to_string();
    let pad = " ".repeat(number.len());
    let before: String =
        line.chars().take(line[..spot.column.min(line.len())].chars().count()).map(|c| if c == '\t' { '\t' } else { ' ' }).collect();
    let width = line.get(spot.column..).map_or(1, |rest| rest.chars().take(spot.width.max(1)).count().max(1));
    format!("{pad} |\n{number} | {line}\n{pad} | {before}{}", "^".repeat(width))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The error of a manifest with the package of `app` (three lines) followed by `rest`.
    fn parse(rest: &str) -> Error {
        crate::Manifest::parse(&format!(
            "[package]
name = \"app\"
version = \"0.1.0\"
{rest}"
        ))
        .unwrap_err()
    }

    #[test]
    fn an_unknown_key_suggests_the_closest_and_names_the_table() {
        let text = "[package]
nmae = \"app\"
version = \"0.1.0\"
";
        let err = crate::Manifest::parse(text).unwrap_err();
        let advice = help::advice(&err).expect("advice");
        assert_eq!(advice.suggestions(), ["name"]);
        let help_line = advice.help().unwrap();
        assert!(help_line.contains("`nmae` in `[package]`") && help_line.contains("did you mean `name`?"), "{help_line}");
        assert!(err.to_string().contains("line 2, column 1"), "{err}");
    }

    #[test]
    fn an_unknown_table_is_named_by_the_table_around_it() {
        let err = parse(
            "
[target.jvmm]
",
        );
        let advice = help::advice(&err).expect("advice");
        assert_eq!(advice.suggestions(), ["jvm"]);
        assert!(advice.help().unwrap().contains("in `[target]`"), "{:?}", advice.help());
    }

    #[test]
    fn a_misspelled_top_level_table_is_not_placed_in_itself() {
        let err = parse(
            "
[dependncies]
",
        );
        let advice = help::advice(&err).expect("advice");
        assert_eq!(advice.suggestions(), ["dependencies"]);
        let help_line = advice.help().unwrap();
        assert!(help_line.starts_with("key `dependncies` is not known; did you mean `dependencies`?"), "{help_line}");
    }

    #[test]
    fn an_unknown_key_in_a_dependency_is_located() {
        let err = parse(
            "
[dependencies]
\"org.a:b\" = { version = \"1\", scop = \"test\" }
",
        );
        let advice = help::advice(&err).expect("advice");
        assert_eq!(advice.suggestions(), ["scope"]);
        assert!(err.to_string().contains("error at line 6, column 30"), "{err}");
    }

    #[test]
    fn a_validation_error_shows_the_line() {
        let err = parse(
            "
[target.jvm]
jvm-target = \"abc\"
",
        );
        let text = format!("{err}");
        assert!(text.contains("error at line 6, column 15"), "{text}");
        assert!(text.contains("6 | jvm-target = \"abc\""), "{text}");
        assert!(text.contains("invalid [target.jvm] jvm-target `abc`"), "{text}");
    }

    #[test]
    fn a_far_off_key_names_the_table_and_the_message_lists_the_valid_ones() {
        let err = parse(
            "zzzzzzzz = 1
",
        );
        let help_line = help::advice(&err).unwrap().help().unwrap().to_owned();
        assert!(help_line.contains("key `zzzzzzzz` in `[package]` is not known") && !help_line.contains("did you mean"), "{help_line}");
        assert!(err.to_string().contains("expected one of `name`"), "{err}");
    }
}
