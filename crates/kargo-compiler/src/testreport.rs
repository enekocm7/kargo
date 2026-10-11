//! Parses TeamCity service messages (emitted by the Kotlin/Wasm and
//! Kotlin/Native test runners) into a Cargo-style test report.

use std::io::{BufRead, Write};

use anstyle::{AnsiColor, Style};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestSummary {
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
    pub failures: Vec<(String, String)>,
}

impl TestSummary {
    pub fn success(&self) -> bool {
        self.failed == 0
    }

    pub fn merge(&mut self, other: TestSummary) {
        self.passed += other.passed;
        self.failed += other.failed;
        self.ignored += other.ignored;
        self.failures.extend(other.failures);
    }
}

/// Parses `##teamcity[name key='value' ...]`, returning the message name and attributes.
pub fn parse_message(line: &str) -> Option<(String, Vec<(String, String)>)> {
    let body = line.trim().strip_prefix("##teamcity[")?.strip_suffix(']')?;
    let (name, mut rest) = match body.find(' ') {
        Some(i) => (&body[..i], &body[i + 1..]),
        None => (body, ""),
    };
    let mut attrs = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let eq = rest.find("='")?;
        let key = rest[..eq].to_owned();
        let mut value = String::new();
        let mut chars = rest[eq + 2..].char_indices();
        let mut end = None;
        while let Some((i, c)) = chars.next() {
            match c {
                '|' => match chars.next().map(|(_, c)| c) {
                    Some('n') => value.push('\n'),
                    Some('r') => value.push('\r'),
                    Some('\'') => value.push('\''),
                    Some('|') => value.push('|'),
                    Some('[') => value.push('['),
                    Some(']') => value.push(']'),
                    Some(other) => value.push(other),
                    None => return None,
                },
                '\'' => {
                    end = Some(eq + 2 + i + 1);
                    break;
                }
                other => value.push(other),
            }
        }
        rest = &rest[end?..];
        attrs.push((key, value));
    }
    Some((name.to_owned(), attrs))
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> &'a str {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).unwrap_or("")
}

/// Consumes test-runner output, printing `test Suite.name ... ok` lines and
/// passing through anything that is not a service message.
pub fn report(input: impl BufRead, mut out: impl Write) -> std::io::Result<TestSummary> {
    let ok = AnsiColor::Green.on_default();
    let failed_style: Style = AnsiColor::Red.on_default();
    let ignored_style: Style = AnsiColor::Yellow.on_default();

    let mut summary = TestSummary::default();
    let mut suites: Vec<String> = Vec::new();
    let mut current_failed = false;
    for line in input.lines() {
        let line = line?;
        let Some((name, attrs)) = parse_message(&line) else {
            writeln!(out, "{line}")?;
            continue;
        };
        let test_name = || {
            let suite = suites.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(".");
            let test = attr(&attrs, "name");
            if suite.is_empty() { test.to_owned() } else { format!("{suite}.{test}") }
        };
        match name.as_str() {
            "testSuiteStarted" => suites.push(attr(&attrs, "name").to_owned()),
            "testSuiteFinished" => {
                suites.pop();
            }
            "testStarted" => current_failed = false,
            "testFailed" => {
                current_failed = true;
                let message = attr(&attrs, "message");
                let details = attr(&attrs, "details");
                let text = if details.is_empty() { message.to_owned() } else { format!("{message}\n{details}") };
                summary.failures.push((test_name(), text));
            }
            "testIgnored" => {
                summary.ignored += 1;
                writeln!(out, "test {} ... {ignored_style}ignored{ignored_style:#}", test_name())?;
            }
            "testFinished" => {
                if current_failed {
                    summary.failed += 1;
                    writeln!(out, "test {} ... {failed_style}FAILED{failed_style:#}", test_name())?;
                } else {
                    summary.passed += 1;
                    writeln!(out, "test {} ... {ok}ok{ok:#}", test_name())?;
                }
                current_failed = false;
            }
            "testStdOut" | "testStdErr" => write!(out, "{}", attr(&attrs, "out"))?,
            _ => {}
        }
    }
    Ok(summary)
}

/// Prints failure details and the final `test result:` line.
pub fn print_summary(summary: &TestSummary, mut out: impl Write) -> std::io::Result<()> {
    if !summary.failures.is_empty() {
        writeln!(out, "\nfailures:\n")?;
        for (name, text) in &summary.failures {
            writeln!(out, "---- {name} ----\n{text}\n")?;
        }
    }
    let (style, verdict) = if summary.success() { (AnsiColor::Green.on_default(), "ok") } else { (AnsiColor::Red.on_default(), "FAILED") };
    writeln!(
        out,
        "\ntest result: {style}{verdict}{style:#}. {} passed; {} failed; {} ignored",
        summary.passed, summary.failed, summary.ignored
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_escaped_attributes() {
        let (name, attrs) = parse_message("##teamcity[testFailed name='a|'b' message='x|ny |[1|]' flowId='f']").unwrap();
        assert_eq!(name, "testFailed");
        assert_eq!(attrs[0], ("name".into(), "a'b".into()));
        assert_eq!(attrs[1], ("message".into(), "x\ny [1]".into()));
        assert!(parse_message("plain output").is_none());
        assert!(parse_message("##teamcity[broken name='unterminated]").is_none());
        assert_eq!(parse_message("##teamcity[noAttrs]").unwrap().0, "noAttrs");
    }

    #[test]
    fn builds_report() {
        let input = "\
##teamcity[testSuiteStarted name='' flowId='1']
##teamcity[testSuiteStarted name='MathTest' flowId='1']
##teamcity[testStarted name='adds' flowId='1']
##teamcity[testFinished name='adds' flowId='1']
##teamcity[testStarted name='fails' flowId='1']
hello from test
##teamcity[testFailed name='fails' message='Expected <4>, actual <3>.' details='trace|nline2' flowId='1']
##teamcity[testFinished name='fails' flowId='1']
##teamcity[testIgnored name='skipped' flowId='1']
##teamcity[testSuiteFinished name='MathTest' flowId='1']
##teamcity[testSuiteFinished name='' flowId='1']
";
        let mut out = Vec::new();
        let summary = report(input.as_bytes(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!((summary.passed, summary.failed, summary.ignored), (1, 1, 1));
        assert!(!summary.success());
        assert!(text.contains("test MathTest.adds ... "), "{text}");
        assert!(text.contains("hello from test"));
        assert_eq!(summary.failures[0].0, "MathTest.fails");
        assert!(summary.failures[0].1.contains("trace\nline2"));

        let mut out = Vec::new();
        print_summary(&summary, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("---- MathTest.fails ----"));
        assert!(text.contains("1 passed; 1 failed; 1 ignored"));

        let mut total = TestSummary::default();
        total.merge(summary);
        assert_eq!(total.failed, 1);
    }
}
