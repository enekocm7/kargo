//! Parses TeamCity service messages (emitted by the Kotlin/Wasm and
//! Kotlin/Native test runners) and JUnit XML reports (written by Gradle for
//! Android unit tests) into a Cargo-style test report, and writes JUnit XML
//! reports for `kargo test --report junit`. With `--message-format json` the
//! results also become `test-started` / `test-finished` JSON messages ([`event_json`]).

use std::io::{BufRead, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use anstyle::{AnsiColor, Style};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestSummary {
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
    pub failures: Vec<(String, String)>,
    /// The test process failed without reporting a failed test (it crashed).
    pub crashed: bool,
    /// Every test that ran, in order, for JUnit XML reports.
    pub cases: Vec<TestCase>,
}

/// One test's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCase {
    /// The enclosing class or suite (`package.Class`); may be empty.
    pub class: String,
    pub name: String,
    pub time: Duration,
    pub outcome: CaseOutcome,
    /// What the test printed.
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaseOutcome {
    Passed,
    Failed { message: String, details: String },
    Ignored,
}

impl TestSummary {
    pub fn success(&self) -> bool {
        self.failed == 0 && !self.crashed
    }

    /// Whether the runner reported any test at all.
    pub fn ran_tests(&self) -> bool {
        self.passed + self.failed + self.ignored > 0
    }

    pub fn merge(&mut self, other: TestSummary) {
        self.passed += other.passed;
        self.failed += other.failed;
        self.ignored += other.ignored;
        self.failures.extend(other.failures);
        self.crashed |= other.crashed;
        self.cases.extend(other.cases);
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

/// A test starting or ending, as the report reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestEvent<'a> {
    Started {
        class: &'a str,
        name: &'a str,
    },
    /// Ignored tests end without starting.
    Finished(&'a TestCase),
}

/// The `test-started` or `test-finished` JSON message for `event` (without the package
/// and target, which the caller adds). The id of a test is `Class.name`.
pub fn event_json(event: TestEvent<'_>) -> serde_json::Value {
    let fields = |reason: &str, class: &str, name: &str| {
        let id = if class.is_empty() { name.to_owned() } else { format!("{class}.{name}") };
        serde_json::json!({ "reason": reason, "id": id, "suite": class, "name": name, "display-name": name })
    };
    match event {
        TestEvent::Started { class, name } => fields("test-started", class, name),
        TestEvent::Finished(case) => {
            let mut json = fields("test-finished", &case.class, &case.name);
            let outcome = match &case.outcome {
                CaseOutcome::Passed => "passed",
                CaseOutcome::Failed { .. } => "failed",
                CaseOutcome::Ignored => "ignored",
            };
            json["outcome"] = outcome.into();
            json["duration-ms"] = u64::try_from(case.time.as_millis()).unwrap_or(u64::MAX).into();
            if let CaseOutcome::Failed { message, details } = &case.outcome {
                json["message"] = message.as_str().into();
                json["details"] = details.as_str().into();
            }
            if !case.output.is_empty() {
                json["stdout"] = case.output.as_str().into();
            }
            json
        }
    }
}

/// Consumes test-runner output, printing `test Suite.name ... ok` lines and
/// passing through anything that is not a service message.
pub fn report(input: impl BufRead, out: impl Write) -> std::io::Result<TestSummary> {
    report_with(input, out, |_| {})
}

/// [`report`], calling `on_event` as each test starts and ends.
pub fn report_with(input: impl BufRead, mut out: impl Write, mut on_event: impl FnMut(TestEvent<'_>)) -> std::io::Result<TestSummary> {
    let ok = AnsiColor::Green.on_default();
    let failed_style: Style = AnsiColor::Red.on_default();
    let ignored_style: Style = AnsiColor::Yellow.on_default();

    let mut summary = TestSummary::default();
    let mut suites: Vec<String> = Vec::new();
    let mut current_failure: Option<(String, String)> = None;
    let mut started = Instant::now();
    // What the current test printed.
    let mut output = String::new();
    for line in input.lines() {
        let line = line?;
        let Some((name, attrs)) = parse_message(&line) else {
            writeln!(out, "{line}")?;
            output.push_str(&line);
            output.push('\n');
            continue;
        };
        let suite = suites.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(".");
        let test = attr(&attrs, "name");
        let full_name = if suite.is_empty() { test.to_owned() } else { format!("{suite}.{test}") };
        let case = |outcome, time, output| TestCase { class: suite.clone(), name: test.to_owned(), time, outcome, output };
        match name.as_str() {
            "testSuiteStarted" => suites.push(test.to_owned()),
            "testSuiteFinished" => {
                suites.pop();
            }
            "testStarted" => {
                on_event(TestEvent::Started { class: &suite, name: test });
                current_failure = None;
                started = Instant::now();
                output.clear();
            }
            "testFailed" => {
                let message = attr(&attrs, "message");
                let details = attr(&attrs, "details");
                let text = if details.is_empty() { message.to_owned() } else { format!("{message}\n{details}") };
                summary.failures.push((full_name.clone(), text));
                current_failure = Some((message.to_owned(), details.to_owned()));
            }
            "testIgnored" => {
                summary.ignored += 1;
                summary.cases.push(case(CaseOutcome::Ignored, Duration::ZERO, String::new()));
                on_event(TestEvent::Finished(summary.cases.last().expect("just pushed")));
                writeln!(out, "test {full_name} ... {ignored_style}ignored{ignored_style:#}")?;
            }
            "testFinished" => {
                // Prefer the runner's own `duration` (milliseconds) over the time between messages.
                let time = attr(&attrs, "duration").parse().map(Duration::from_millis).unwrap_or_else(|_| started.elapsed());
                let output = std::mem::take(&mut output);
                if let Some((message, details)) = current_failure.take() {
                    summary.failed += 1;
                    summary.cases.push(case(CaseOutcome::Failed { message, details }, time, output));
                    writeln!(out, "test {full_name} ... {failed_style}FAILED{failed_style:#}")?;
                } else {
                    summary.passed += 1;
                    summary.cases.push(case(CaseOutcome::Passed, time, output));
                    writeln!(out, "test {full_name} ... {ok}ok{ok:#}")?;
                }
                on_event(TestEvent::Finished(summary.cases.last().expect("just pushed")));
            }
            "testStdOut" | "testStdErr" => {
                let text = attr(&attrs, "out");
                write!(out, "{text}")?;
                output.push_str(text);
            }
            _ => {}
        }
    }
    Ok(summary)
}

/// Reads one JUnit XML report (`<testsuite>` or `<testsuites>`), printing a
/// `test Class.name ... ok` line per test case.
pub fn report_junit_xml(xml: &str, out: impl Write) -> std::io::Result<TestSummary> {
    report_junit_xml_with(xml, out, |_| {})
}

/// [`report_junit_xml`], calling `on_event` for each test case (they have all ended).
pub fn report_junit_xml_with(xml: &str, mut out: impl Write, mut on_event: impl FnMut(TestEvent<'_>)) -> std::io::Result<TestSummary> {
    let ok = AnsiColor::Green.on_default();
    let failed_style: Style = AnsiColor::Red.on_default();
    let ignored_style: Style = AnsiColor::Yellow.on_default();

    let doc = roxmltree::Document::parse(xml).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut summary = TestSummary::default();
    for case in doc.descendants().filter(|n| n.has_tag_name("testcase")) {
        let class = case.attribute("classname").unwrap_or_default();
        let name = case.attribute("name").unwrap_or_default();
        let full = if class.is_empty() { name.to_owned() } else { format!("{class}.{name}") };
        let failure = case.children().find(|c| c.has_tag_name("failure") || c.has_tag_name("error"));
        let outcome = if let Some(failure) = failure {
            summary.failed += 1;
            let message = failure.attribute("message").unwrap_or_default();
            let details = failure.text().map(str::trim).unwrap_or_default();
            let text = if details.is_empty() { message } else { details };
            summary.failures.push((full.clone(), text.to_owned()));
            writeln!(out, "test {full} ... {failed_style}FAILED{failed_style:#}")?;
            CaseOutcome::Failed { message: message.to_owned(), details: details.to_owned() }
        } else if case.children().any(|c| c.has_tag_name("skipped")) {
            summary.ignored += 1;
            writeln!(out, "test {full} ... {ignored_style}ignored{ignored_style:#}")?;
            CaseOutcome::Ignored
        } else {
            summary.passed += 1;
            writeln!(out, "test {full} ... {ok}ok{ok:#}")?;
            CaseOutcome::Passed
        };
        let seconds = case.attribute("time").and_then(|t| t.parse::<f64>().ok()).filter(|t| t.is_finite() && *t >= 0.0);
        let output = case.children().find(|c| c.has_tag_name("system-out")).and_then(|c| c.text()).unwrap_or_default();
        summary.cases.push(TestCase {
            class: class.to_owned(),
            name: name.to_owned(),
            time: seconds.map(Duration::from_secs_f64).unwrap_or_default(),
            outcome,
            output: output.to_owned(),
        });
        on_event(TestEvent::Finished(summary.cases.last().expect("just pushed")));
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

/// Renders `cases` as a JUnit XML document with one `<testsuite>` per class,
/// the format CI systems (GitHub Actions, GitLab, Jenkins) read.
pub fn junit_xml(name: &str, cases: &[TestCase]) -> String {
    let mut classes: Vec<&str> = Vec::new();
    for case in cases {
        if !classes.contains(&case.class.as_str()) {
            classes.push(&case.class);
        }
    }
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let all: Vec<&TestCase> = cases.iter().collect();
    xml.push_str(&format!("<testsuites name=\"{}\"{}>\n", escape(name), counts(&all)));
    for class in classes {
        let members: Vec<&TestCase> = cases.iter().filter(|c| c.class == class).collect();
        let suite = escape(if class.is_empty() { name } else { class });
        xml.push_str(&format!("  <testsuite name=\"{suite}\"{}>\n", counts(&members)));
        for case in members {
            let open =
                format!("    <testcase name=\"{}\" classname=\"{suite}\" time=\"{:.3}\"", escape(&case.name), case.time.as_secs_f64());
            if case.outcome == CaseOutcome::Passed && case.output.is_empty() {
                xml.push_str(&format!("{open}/>\n"));
                continue;
            }
            xml.push_str(&format!("{open}>\n"));
            match &case.outcome {
                CaseOutcome::Passed => {}
                CaseOutcome::Ignored => xml.push_str("      <skipped/>\n"),
                CaseOutcome::Failed { message, details } => {
                    xml.push_str(&format!("      <failure message=\"{}\">{}</failure>\n", escape(message), escape(details)))
                }
            }
            if !case.output.is_empty() {
                xml.push_str(&format!("      <system-out>{}</system-out>\n", escape(&case.output)));
            }
            xml.push_str("    </testcase>\n");
        }
        xml.push_str("  </testsuite>\n");
    }
    xml.push_str("</testsuites>\n");
    xml
}

/// The `tests`, `failures`, `errors`, `skipped` and `time` attributes of a suite.
fn counts(cases: &[&TestCase]) -> String {
    let failures = cases.iter().filter(|c| matches!(c.outcome, CaseOutcome::Failed { .. })).count();
    let skipped = cases.iter().filter(|c| c.outcome == CaseOutcome::Ignored).count();
    let time = cases.iter().map(|c| c.time).sum::<Duration>().as_secs_f64();
    format!(" tests=\"{}\" failures=\"{failures}\" errors=\"0\" skipped=\"{skipped}\" time=\"{time:.3}\"", cases.len())
}

/// Writes [`junit_xml`] to `path`, creating its directory.
pub fn write_junit_xml(path: &Path, name: &str, cases: &[TestCase]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, junit_xml(name, cases))
}

/// Escapes text for XML attributes and content, dropping the control characters
/// XML 1.0 can't hold (such as the escape in ANSI-colored test output).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' | '\r' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
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
##teamcity[testFinished name='adds' duration='12' flowId='1']
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
        assert_eq!(summary.cases.len(), 3);
        assert_eq!((summary.cases[0].class.as_str(), summary.cases[0].name.as_str()), ("MathTest", "adds"));
        assert_eq!(summary.cases[0].time, Duration::from_millis(12));
        assert_eq!(summary.cases[1].output, "hello from test\n");
        assert!(matches!(&summary.cases[1].outcome, CaseOutcome::Failed { message, .. } if message == "Expected <4>, actual <3>."));
        assert_eq!(summary.cases[2].outcome, CaseOutcome::Ignored);

        let mut out = Vec::new();
        print_summary(&summary, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("---- MathTest.fails ----"));
        assert!(text.contains("1 passed; 1 failed; 1 ignored"));

        let crashed = TestSummary { passed: 2, crashed: true, ..TestSummary::default() };
        assert!(!crashed.success());
        let mut out = Vec::new();
        print_summary(&crashed, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("FAILED"));
        assert!(!TestSummary::default().ran_tests());

        let mut total = TestSummary::default();
        total.merge(summary);
        assert_eq!(total.failed, 1);
    }

    #[test]
    fn reports_events_as_tests_run() {
        let input = "\
##teamcity[testSuiteStarted name='MathTest' flowId='1']
##teamcity[testStarted name='adds' flowId='1']
printed
##teamcity[testFinished name='adds' duration='12' flowId='1']
##teamcity[testStarted name='fails' flowId='1']
##teamcity[testFailed name='fails' message='boom' details='trace' flowId='1']
##teamcity[testFinished name='fails' flowId='1']
##teamcity[testIgnored name='skipped' flowId='1']
##teamcity[testSuiteFinished name='MathTest' flowId='1']
";
        let mut events = Vec::new();
        report_with(input.as_bytes(), Vec::new(), |event| events.push(event_json(event))).unwrap();
        let reasons: Vec<&str> = events.iter().map(|e| e["reason"].as_str().unwrap()).collect();
        assert_eq!(reasons, ["test-started", "test-finished", "test-started", "test-finished", "test-finished"]);
        assert_eq!(
            events[1],
            serde_json::json!({
                "reason": "test-finished", "id": "MathTest.adds", "suite": "MathTest", "name": "adds", "display-name": "adds",
                "outcome": "passed", "duration-ms": 12, "stdout": "printed\n",
            })
        );
        assert_eq!((events[3]["outcome"].as_str(), events[3]["message"].as_str()), (Some("failed"), Some("boom")));
        assert_eq!(events[3]["details"], "trace");
        assert_eq!(events[4]["outcome"], "ignored");

        let xml = r#"<testsuite><testcase name="a" classname="C" time="0.5"/></testsuite>"#;
        let mut events = Vec::new();
        report_junit_xml_with(xml, Vec::new(), |event| events.push(event_json(event))).unwrap();
        assert_eq!((events[0]["id"].as_str(), events[0]["duration-ms"].as_u64()), (Some("C.a"), Some(500)));
    }

    #[test]
    fn reads_junit_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="GreetingTest" tests="3" skipped="1" failures="1" errors="0">
  <testcase name="greets" classname="GreetingTest" time="0.01"/>
  <testcase name="fails" classname="GreetingTest" time="0.01">
    <failure message="expected:&lt;a&gt; but was:&lt;b&gt;" type="java.lang.AssertionError">java.lang.AssertionError: expected
	at GreetingTest.fails(GreetingTest.kt:9)</failure>
  </testcase>
  <testcase name="later" classname="GreetingTest" time="0">
    <skipped/>
  </testcase>
  <system-out><![CDATA[]]></system-out>
</testsuite>"#;
        let mut out = Vec::new();
        let summary = report_junit_xml(xml, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!((summary.passed, summary.failed, summary.ignored), (1, 1, 1));
        assert!(text.contains("test GreetingTest.greets ... "), "{text}");
        assert_eq!(summary.failures[0].0, "GreetingTest.fails");
        assert!(summary.failures[0].1.contains("GreetingTest.kt:9"));
        assert!(report_junit_xml("<testsuite", Vec::new()).is_err());
        assert_eq!(summary.cases[0].time, Duration::from_millis(10));
    }

    #[test]
    fn writes_junit_xml() {
        let case = |class: &str, name: &str, outcome| TestCase {
            class: class.into(),
            name: name.into(),
            time: Duration::from_millis(1500),
            outcome,
            output: String::new(),
        };
        let mut cases = vec![
            case("app.MathTest", "adds", CaseOutcome::Passed),
            case("app.MathTest", "fails", CaseOutcome::Failed { message: "a < b".into(), details: "trace\u{1b}[0m".into() }),
            case("app.Other", "later", CaseOutcome::Ignored),
        ];
        cases[0].output = "printed\n".into();
        let xml = junit_xml("demo (wasm)", &cases);
        assert!(xml.contains(r#"<testsuites name="demo (wasm)" tests="3" failures="1" errors="0" skipped="1" time="4.500">"#), "{xml}");
        assert!(xml.contains(r#"<testsuite name="app.Other" tests="1" failures="0" errors="0" skipped="1" time="1.500">"#), "{xml}");
        assert!(xml.contains(r#"<failure message="a &lt; b">trace[0m</failure>"#), "{xml}");

        // The report reads back with the same results.
        let summary = report_junit_xml(&xml, Vec::new()).unwrap();
        assert_eq!((summary.passed, summary.failed, summary.ignored), (1, 1, 1));
        assert_eq!(summary.cases[0], cases[0]);
        assert_eq!(summary.cases[2], cases[2]);
    }
}
