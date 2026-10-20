//! Benchmark results: reading JMH's JSON, comparing two runs and printing the report of `kargo bench`.
//!
//! JMH writes one object per benchmark and mode, with the score, its error and the score of
//! every iteration (`rawData`). A comparison tests the iteration scores of both runs with
//! Welch's t-test, so that noise on a machine is not reported as a change.

use std::collections::BTreeMap;
use std::io::Write;

use anstyle::{AnsiColor, Style};
use anyhow::{Context, Result, bail};
use serde_json::Value;

/// A change is reported as one when the p-value of the comparison is below this.
pub const SIGNIFICANCE: f64 = 0.05;

/// One benchmark in one mode, as JMH measured it.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchResult {
    /// The benchmark's name: `package.Class.method`.
    pub benchmark: String,
    /// `thrpt`, `avgt`, `sample` or `ss`.
    pub mode: String,
    /// The `@Param` values this run used.
    pub params: BTreeMap<String, String>,
    pub score: f64,
    /// The half width of the 99.9% confidence interval of the score; `0` when JMH had too few iterations for one.
    pub error: f64,
    /// JMH's unit of `score`, such as `ns/op` or `ops/s`.
    pub unit: String,
    /// The score of each measured iteration, over all forks.
    pub samples: Vec<f64>,
}

impl BenchResult {
    /// What identifies a benchmark between two runs: its name, mode and parameters.
    pub fn key(&self) -> String {
        format!("{}|{}|{}", self.benchmark, self.mode, self.params_text())
    }

    /// The name with the parameters, such as `com.example.Sort.sort (size = 100)`.
    pub fn label(&self) -> String {
        match self.params_text() {
            params if params.is_empty() => self.benchmark.clone(),
            params => format!("{} ({params})", self.benchmark),
        }
    }

    fn params_text(&self) -> String {
        self.params.iter().map(|(k, v)| format!("{k} = {v}")).collect::<Vec<_>>().join(", ")
    }

    /// The score in the base unit of its kind (seconds or operations per second).
    fn measure(&self) -> Option<Measure> {
        let (kind, factor) = unit_scale(&self.unit)?;
        Some(Measure {
            kind,
            score: kind.to_base(self.score, factor),
            error: kind.to_base(self.error, factor),
            samples: self.samples.iter().map(|s| kind.to_base(*s, factor)).collect(),
        })
    }
}

/// Reads the JSON array JMH writes with `-rf json`.
pub fn parse_results(json: &str) -> Result<Vec<BenchResult>> {
    let value: Value = serde_json::from_str(json).context("the benchmark results are not valid JSON")?;
    let entries = value.as_array().context("the benchmark results should be a JSON array")?;
    let mut results = Vec::new();
    for entry in entries {
        let benchmark = entry["benchmark"].as_str().context("a benchmark result has no `benchmark` name")?;
        let metric = &entry["primaryMetric"];
        let params = entry["params"]
            .as_object()
            .map(|params| params.iter().map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), str::to_owned))).collect())
            .unwrap_or_default();
        let samples = metric["rawData"]
            .as_array()
            .map(|forks| forks.iter().filter_map(Value::as_array).flatten().filter_map(number).collect())
            .unwrap_or_default();
        results.push(BenchResult {
            benchmark: benchmark.to_owned(),
            mode: entry["mode"].as_str().unwrap_or("thrpt").to_owned(),
            params,
            score: number(&metric["score"]).with_context(|| format!("`{benchmark}` has no score"))?,
            error: number(&metric["scoreError"]).filter(|e| e.is_finite()).unwrap_or(0.0),
            unit: metric["scoreUnit"].as_str().unwrap_or("").to_owned(),
            samples,
        });
    }
    Ok(results)
}

/// A JSON number, or the string `"NaN"` JMH writes for a value it could not compute.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Whether a bigger score is better.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Time per operation: `ns/op` and the like. Smaller is better.
    Time,
    /// Operations per time: `ops/s` and the like. Bigger is better.
    Throughput,
}

impl Kind {
    fn to_base(self, value: f64, factor: f64) -> f64 {
        value * factor
    }

    fn higher_is_better(self) -> bool {
        self == Kind::Throughput
    }
}

/// A score in seconds (per operation) or operations per second.
struct Measure {
    kind: Kind,
    score: f64,
    error: f64,
    samples: Vec<f64>,
}

/// The kind of a JMH unit and what to multiply a value in it by to get seconds, or operations per second.
fn unit_scale(unit: &str) -> Option<(Kind, f64)> {
    let time = |unit: &str| {
        Some(match unit {
            "ns" => 1e-9,
            "us" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            "d" => 86400.0,
            _ => return None,
        })
    };
    if let Some(per) = unit.strip_prefix("ops/") {
        // `ops/ms` is a thousand operations per second.
        return time(per).map(|seconds| (Kind::Throughput, 1.0 / seconds));
    }
    time(unit.strip_suffix("/op")?).map(|seconds| (Kind::Time, seconds))
}

/// How a benchmark's score changed between two runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Change {
    /// The new score relative to the old one, in percent. Negative means a smaller score.
    pub percent: f64,
    /// The probability of a difference this big if nothing changed; `None` without enough iterations to test.
    pub p_value: Option<f64>,
    pub verdict: Verdict,
}

/// Whether a benchmark got better or worse, counting only differences that are not noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Faster,
    Slower,
    Unchanged,
}

/// Compares `new` with `old`. `None` when the two cannot be compared: their units are not of the same
/// kind (time per operation against operations per time) or are unknown, or the old score is zero.
pub fn compare(old: &BenchResult, new: &BenchResult) -> Option<Change> {
    let (a, b) = (old.measure()?, new.measure()?);
    if a.kind != b.kind || a.score == 0.0 {
        return None;
    }
    let percent = (b.score - a.score) / a.score * 100.0;
    let p_value = welch_p(&a.samples, &b.samples);
    let different = match p_value {
        Some(p) => p < SIGNIFICANCE,
        // Too few iterations for a test: a change counts when the error intervals do not overlap.
        None => (b.score - a.score).abs() > a.error + b.error,
    };
    let verdict = if !different || b.score == a.score {
        Verdict::Unchanged
    } else if (b.score > a.score) == a.kind.higher_is_better() {
        Verdict::Faster
    } else {
        Verdict::Slower
    };
    Some(Change { percent, p_value, verdict })
}

/// The two-sided p-value of Welch's t-test for a difference between the means of `a` and `b`.
fn welch_p(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() < 2 || b.len() < 2 {
        return None;
    }
    let stats = |xs: &[f64]| {
        let n = xs.len() as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let variance = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
        (n, mean, variance / n)
    };
    let ((n1, m1, e1), (n2, m2, e2)) = (stats(a), stats(b));
    let error = e1 + e2;
    if error == 0.0 {
        // Every iteration scored the same in each run.
        return Some(if m1 == m2 { 1.0 } else { 0.0 });
    }
    let t = (m1 - m2) / error.sqrt();
    let degrees = error * error / (e1 * e1 / (n1 - 1.0) + e2 * e2 / (n2 - 1.0));
    Some(incomplete_beta(degrees / 2.0, 0.5, degrees / (degrees + t * t)))
}

/// The regularized incomplete beta function `I_x(a, b)`, by Lentz's continued fraction.
fn incomplete_beta(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let front = (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) { front * beta_fraction(a, b, x) / a } else { 1.0 - front * beta_fraction(b, a, 1.0 - x) / b }
}

fn beta_fraction(a: f64, b: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=300 {
        let m = m as f64;
        let m2 = 2.0 * m;
        let even = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + even * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + even / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;
        let odd = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + odd * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + odd / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let delta = d * c;
        h *= delta;
        if (delta - 1.0).abs() < 1e-15 {
            break;
        }
    }
    h
}

/// The natural logarithm of the gamma function, by the Lanczos approximation.
fn ln_gamma(x: f64) -> f64 {
    const COEFFICIENTS: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Reflection: Γ(x)Γ(1-x) = π / sin(πx).
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let t = x + 7.5;
    let sum = COEFFICIENTS[0] + COEFFICIENTS[1..].iter().enumerate().map(|(i, c)| c / (x + i as f64 + 1.0)).sum::<f64>();
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + sum.ln()
}

/// How scores are written: in the unit JMH reported them in, or in whichever unit reads best.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    /// `1.50 us` rather than `0.00150 ms`: each score picks its own unit.
    Auto,
    /// The unit the benchmark or `[bench] time-unit` asked for.
    AsReported,
}

/// `score` in `unit` as a short text, such as `1.23 us` or `4.5 Mops/s`.
fn quantity(value: f64, unit: &str, scale: Scale) -> String {
    let reported = |value: f64| match unit.strip_suffix("/op") {
        Some(time) => format!("{} {time}", significant(value)),
        None => format!("{} {unit}", significant(value)),
    };
    if scale == Scale::AsReported {
        return reported(value);
    }
    let Some((kind, factor)) = unit_scale(unit) else { return reported(value) };
    let base = kind.to_base(value, factor);
    match kind {
        Kind::Time => {
            let (divisor, name) = match base.abs() {
                x if x >= 1.0 || x == 0.0 => (1.0, "s"),
                x if x >= 1e-3 => (1e-3, "ms"),
                x if x >= 1e-6 => (1e-6, "us"),
                _ => (1e-9, "ns"),
            };
            format!("{} {name}", significant(base / divisor))
        }
        Kind::Throughput => {
            let (divisor, name) = match base.abs() {
                x if x >= 1e9 => (1e9, "Gops/s"),
                x if x >= 1e6 => (1e6, "Mops/s"),
                x if x >= 1e3 => (1e3, "Kops/s"),
                _ => (1.0, "ops/s"),
            };
            format!("{} {name}", significant(base / divisor))
        }
    }
}

/// `value` with about three significant digits: `123`, `12.3`, `1.23`, `0.123`.
fn significant(value: f64) -> String {
    if !value.is_finite() {
        return "NaN".to_owned();
    }
    let decimals = match value.abs() {
        x if x >= 100.0 || x == 0.0 => 0,
        x if x >= 10.0 => 1,
        x if x >= 1.0 => 2,
        _ => 3,
    };
    format!("{value:.decimals$}")
}

/// The score of `result` and its error, as `1.23 us` and `0.04 us` (empty without an error).
fn score_and_error(result: &BenchResult, scale: Scale) -> (String, String) {
    let score = quantity(result.score, &result.unit, scale);
    let error = if result.error > 0.0 { quantity_in(result.error, result, &score, scale) } else { String::new() };
    (score, error)
}

/// The error in the unit its score was written in, so `1.23 us ± 0.04 us` and not `± 40 ns`.
fn quantity_in(error: f64, result: &BenchResult, score_text: &str, scale: Scale) -> String {
    if scale == Scale::Auto
        && let (Some((kind, factor)), Some(name)) = (unit_scale(&result.unit), score_text.split_once(' ').map(|(_, unit)| unit))
    {
        let base = kind.to_base(error, factor);
        let divisor = match (kind, name) {
            (Kind::Time, "s") => 1.0,
            (Kind::Time, "ms") => 1e-3,
            (Kind::Time, "us") => 1e-6,
            (Kind::Time, "ns") => 1e-9,
            (Kind::Throughput, "Gops/s") => 1e9,
            (Kind::Throughput, "Mops/s") => 1e6,
            (Kind::Throughput, "Kops/s") => 1e3,
            _ => 1.0,
        };
        return format!("{} {name}", significant(base / divisor));
    }
    quantity(error, &result.unit, scale)
}

const BOLD: Style = Style::new().bold();
const GOOD: Style = AnsiColor::Green.on_default();
const BAD: Style = AnsiColor::Red.on_default();

/// Prints the results as a table: the benchmark, its mode, its score and the error of the score.
pub fn write_table(mut out: impl Write, results: &[BenchResult], scale: Scale) -> std::io::Result<()> {
    let rows: Vec<[String; 4]> = results
        .iter()
        .map(|r| {
            let (score, error) = score_and_error(r, scale);
            [r.label(), r.mode.clone(), score, if error.is_empty() { error } else { format!("± {error}") }]
        })
        .collect();
    let headers = ["benchmark", "mode", "score", "error"];
    let widths: Vec<usize> =
        (0..4).map(|i| rows.iter().map(|row| row[i].chars().count()).chain([headers[i].len()]).max().unwrap_or(0)).collect();
    writeln!(
        out,
        "{BOLD}{:<w0$}  {:<w1$}  {:>w2$}  {:<w3$}{BOLD:#}",
        headers[0],
        headers[1],
        headers[2],
        headers[3],
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
        w3 = widths[3]
    )?;
    for row in rows {
        let line = format!(
            "{:<w0$}  {:<w1$}  {:>w2$}  {:<w3$}",
            row[0],
            row[1],
            row[2],
            row[3],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3]
        );
        writeln!(out, "{}", line.trim_end())?;
    }
    Ok(())
}

/// A line of a comparison: the benchmark, its old and new score and how it changed, if it can be compared.
type Row = (String, String, String, Option<(Style, String)>);

/// How a run compares with a baseline, benchmark by benchmark.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub faster: usize,
    pub slower: usize,
    pub unchanged: usize,
    /// Benchmarks the baseline does not have, or that cannot be compared with its.
    pub new: usize,
}

/// Prints each of `new` next to the baseline's score, such as
/// `parse  1.20 ms -> 1.05 ms (-12.5%, p < 0.05)`.
pub fn write_comparison(mut out: impl Write, baseline: &[BenchResult], new: &[BenchResult], scale: Scale) -> std::io::Result<Summary> {
    let old: BTreeMap<String, &BenchResult> = baseline.iter().map(|r| (r.key(), r)).collect();
    let mut summary = Summary::default();
    let mut rows: Vec<Row> = Vec::new();
    for result in new {
        let (score, _) = score_and_error(result, scale);
        match old.get(&result.key()).and_then(|old| compare(old, result).map(|change| (*old, change))) {
            Some((old, change)) => {
                let style = match change.verdict {
                    Verdict::Faster => {
                        summary.faster += 1;
                        GOOD
                    }
                    Verdict::Slower => {
                        summary.slower += 1;
                        BAD
                    }
                    Verdict::Unchanged => {
                        summary.unchanged += 1;
                        Style::new()
                    }
                };
                rows.push((result.label(), score_and_error(old, scale).0, score, Some((style, describe_change(&change)))));
            }
            None => {
                summary.new += 1;
                rows.push((result.label(), String::new(), score, None));
            }
        }
    }
    let width = |f: fn(&Row) -> usize| rows.iter().map(f).max().unwrap_or(0);
    let (w0, w1, w2) = (width(|r| r.0.chars().count()), width(|r| r.1.chars().count()), width(|r| r.2.chars().count()));
    for (name, old, new, change) in rows {
        match change {
            Some((style, text)) => writeln!(out, "{name:<w0$}  {old:>w1$} -> {new:>w2$}  {style}{text}{style:#}")?,
            None => writeln!(out, "{name:<w0$}  {:>w1$}    {new:>w2$}  (not in the baseline)", "")?,
        }
    }
    Ok(summary)
}

fn describe_change(change: &Change) -> String {
    let percent = if change.percent.abs() < 0.05 { "0.0".to_owned() } else { format!("{:+.1}", change.percent) };
    match (change.p_value, change.verdict) {
        (Some(_), Verdict::Faster | Verdict::Slower) => format!("({percent}%, p < {SIGNIFICANCE})"),
        (Some(p), Verdict::Unchanged) => format!("({percent}%, p = {p:.2}, no significant change)"),
        (None, Verdict::Unchanged) => format!("({percent}%, within the margin of error)"),
        (None, _) => format!("({percent}%)"),
    }
}

/// A baseline's name is a file name under `target/bench/`.
pub fn validate_baseline_name(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) || name.starts_with('.') {
        bail!("invalid baseline name `{name}`; use letters, digits, `-`, `_` and `.`, such as `main` or `v1.2`");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(name: &str, score: f64, unit: &str, samples: &[f64]) -> BenchResult {
        BenchResult {
            benchmark: name.to_owned(),
            mode: if unit.starts_with("ops/") { "thrpt" } else { "avgt" }.to_owned(),
            params: BTreeMap::new(),
            score,
            error: 0.0,
            unit: unit.to_owned(),
            samples: samples.to_vec(),
        }
    }

    const JMH_JSON: &str = r#"[
      {
        "jmhVersion": "1.37", "benchmark": "com.example.ParseBench.parse", "mode": "avgt", "threads": 1, "forks": 2,
        "params": { "size": "100" },
        "primaryMetric": {
          "score": 1.5E-6, "scoreError": 2.0E-8, "scoreConfidence": [1.4E-6, 1.6E-6], "scoreUnit": "s/op",
          "rawData": [[1.4E-6, 1.6E-6], [1.5E-6, 1.5E-6]]
        },
        "secondaryMetrics": {}
      },
      {
        "benchmark": "com.example.ParseBench.once", "mode": "thrpt",
        "primaryMetric": { "score": 1000.0, "scoreError": "NaN", "scoreUnit": "ops/s", "rawData": [[1000.0]] }
      }
    ]"#;

    #[test]
    fn reads_jmh_json() {
        let results = parse_results(JMH_JSON).unwrap();
        assert_eq!(results.len(), 2);
        let parse = &results[0];
        assert_eq!((parse.benchmark.as_str(), parse.mode.as_str(), parse.unit.as_str()), ("com.example.ParseBench.parse", "avgt", "s/op"));
        assert_eq!(parse.samples, [1.4e-6, 1.6e-6, 1.5e-6, 1.5e-6]);
        assert_eq!(parse.label(), "com.example.ParseBench.parse (size = 100)");
        assert_ne!(parse.key(), results[1].key());
        assert_eq!(results[1].error, 0.0, "NaN means no error was computed");
        assert!(parse_results("{}").is_err());
        assert!(parse_results("nope").is_err());
        assert!(parse_results(r#"[{"mode":"avgt"}]"#).is_err());
    }

    #[test]
    fn units_convert_to_a_base() {
        assert_eq!(unit_scale("ns/op"), Some((Kind::Time, 1e-9)));
        assert_eq!(unit_scale("ms/op"), Some((Kind::Time, 1e-3)));
        assert_eq!(unit_scale("ops/s"), Some((Kind::Throughput, 1.0)));
        assert_eq!(unit_scale("ops/ms"), Some((Kind::Throughput, 1000.0)));
        assert_eq!(unit_scale("bytes/op"), None);
        assert_eq!(unit_scale("%"), None);
    }

    #[test]
    fn scores_read_in_a_fitting_unit() {
        assert_eq!(quantity(1.5e-6, "s/op", Scale::Auto), "1.50 us");
        assert_eq!(quantity(1500.0, "ns/op", Scale::Auto), "1.50 us");
        assert_eq!(quantity(0.0012, "s/op", Scale::Auto), "1.20 ms");
        assert_eq!(quantity(123.456, "ns/op", Scale::Auto), "123 ns");
        assert_eq!(quantity(2.5, "s/op", Scale::Auto), "2.50 s");
        assert_eq!(quantity(4_500_000.0, "ops/s", Scale::Auto), "4.50 Mops/s");
        assert_eq!(quantity(12.0, "ops/s", Scale::Auto), "12.0 ops/s");
        assert_eq!(quantity(1.5, "ms/op", Scale::AsReported), "1.50 ms");
        assert_eq!(quantity(7.0, "ops/ms", Scale::AsReported), "7.00 ops/ms");
        assert_eq!(quantity(3.0, "bytes/op", Scale::Auto), "3.00 bytes");
    }

    #[test]
    fn t_test_matches_known_p_values() {
        // |t| = 1 with 8 degrees of freedom.
        let p = welch_p(&[1.0, 2.0, 3.0, 4.0, 5.0], &[2.0, 3.0, 4.0, 5.0, 6.0]).unwrap();
        assert!((p - 0.3466).abs() < 1e-3, "{p}");
        // Far apart samples are certainly different, identical ones are not.
        assert!(welch_p(&[1.0, 1.1, 0.9, 1.0], &[5.0, 5.1, 4.9, 5.0]).unwrap() < 1e-4);
        assert!(welch_p(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]).unwrap() > 0.99);
        assert_eq!(welch_p(&[1.0, 1.0], &[1.0, 1.0]), Some(1.0));
        assert_eq!(welch_p(&[1.0, 1.0], &[2.0, 2.0]), Some(0.0));
        assert_eq!(welch_p(&[1.0], &[1.0, 2.0]), None, "one iteration cannot be tested");
        // t = 2.228 is the 5% point with 10 degrees of freedom.
        assert!((incomplete_beta(5.0, 0.5, 10.0 / (10.0 + 2.228 * 2.228)) - 0.05).abs() < 1e-3);
    }

    #[test]
    fn a_faster_run_is_reported_faster() {
        let old = result("b", 1.2e-3, "s/op", &[1.19e-3, 1.21e-3, 1.2e-3, 1.2e-3, 1.2e-3]);
        let new = result("b", 1.05e-3, "s/op", &[1.04e-3, 1.06e-3, 1.05e-3, 1.05e-3, 1.05e-3]);
        let change = compare(&old, &new).unwrap();
        assert_eq!(change.verdict, Verdict::Faster);
        assert!((change.percent + 12.5).abs() < 1e-9, "{}", change.percent);
        assert!(change.p_value.unwrap() < SIGNIFICANCE);
        assert_eq!(describe_change(&change), "(-12.5%, p < 0.05)");
        assert_eq!(compare(&new, &old).unwrap().verdict, Verdict::Slower);
    }

    #[test]
    fn for_throughput_more_is_better() {
        let old = result("b", 100.0, "ops/s", &[99.0, 101.0, 100.0, 100.0]);
        let new = result("b", 150.0, "ops/s", &[149.0, 151.0, 150.0, 150.0]);
        assert_eq!(compare(&old, &new).unwrap().verdict, Verdict::Faster);
        assert_eq!(compare(&new, &old).unwrap().verdict, Verdict::Slower);
    }

    #[test]
    fn noise_is_not_a_change() {
        let old = result("b", 1.0, "ns/op", &[0.8, 1.2, 1.0, 0.9, 1.1]);
        let new = result("b", 1.02, "ns/op", &[1.2, 0.8, 1.1, 0.95, 1.05]);
        let change = compare(&old, &new).unwrap();
        assert_eq!(change.verdict, Verdict::Unchanged);
        assert!(describe_change(&change).contains("no significant change"), "{}", describe_change(&change));
    }

    #[test]
    fn without_iterations_the_error_decides() {
        let mut old = result("b", 10.0, "ns/op", &[]);
        let mut new = result("b", 12.0, "ns/op", &[]);
        old.error = 0.5;
        new.error = 0.5;
        let change = compare(&old, &new).unwrap();
        assert_eq!((change.p_value, change.verdict), (None, Verdict::Slower));
        old.error = 2.0;
        assert_eq!(compare(&old, &new).unwrap().verdict, Verdict::Unchanged);
    }

    #[test]
    fn units_are_compared_in_a_common_base() {
        let old = result("b", 1000.0, "ns/op", &[1000.0, 1001.0, 999.0]);
        let new = result("b", 1.0, "us/op", &[1.0, 1.001, 0.999]);
        let change = compare(&old, &new).unwrap();
        assert_eq!(change.verdict, Verdict::Unchanged);
        assert!(change.percent.abs() < 1e-9);
        assert_eq!(compare(&old, &result("b", 5.0, "ops/s", &[])), None, "a time cannot be compared with a rate");
        assert_eq!(compare(&result("b", 0.0, "ns/op", &[]), &new), None);
    }

    fn render(f: impl FnOnce(&mut Vec<u8>)) -> String {
        let mut out = Vec::new();
        f(&mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_table_lines_up() {
        let mut slow = result("com.example.A.long_name", 2.5e-3, "s/op", &[]);
        slow.error = 1e-4;
        let text = render(|out| write_table(out, &[result("a.B.c", 12.0, "ns/op", &[]), slow], Scale::Auto).unwrap());
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("benchmark") && lines[0].contains("score"), "{text}");
        assert!(lines[1].starts_with("a.B.c"), "{text}");
        assert!(lines[1].contains("avgt") && lines[1].contains("12.0 ns"), "{text}");
        assert!(lines[2].contains("2.50 ms") && lines[2].contains("± 0.100 ms"), "{text}");
        assert!(lines[1].len() < lines[2].len(), "trailing blanks are trimmed");
    }

    #[test]
    fn a_comparison_names_each_change() {
        let old = [
            result("a.fast", 1.2e-3, "s/op", &[1.19e-3, 1.21e-3, 1.2e-3, 1.2e-3]),
            result("a.same", 5.0e-6, "s/op", &[5.0e-6, 5.1e-6, 4.9e-6, 5.0e-6]),
            result("a.gone", 1.0, "s/op", &[]),
        ];
        let new = [
            result("a.fast", 1.05e-3, "s/op", &[1.04e-3, 1.06e-3, 1.05e-3, 1.05e-3]),
            result("a.same", 5.0e-6, "s/op", &[5.0e-6, 5.1e-6, 4.9e-6, 5.0e-6]),
            result("a.added", 3.0e-9, "s/op", &[]),
        ];
        let mut summary = Summary::default();
        let text = render(|out| summary = write_comparison(out, &old, &new, Scale::Auto).unwrap());
        assert_eq!(summary, Summary { faster: 1, slower: 0, unchanged: 1, new: 1 });
        assert!(text.contains("a.fast") && text.contains("1.20 ms -> 1.05 ms") && text.contains("(-12.5%, p < 0.05)"), "{text}");
        assert!(text.contains("a.same") && text.contains("(0.0%, p = 1.00, no significant change)"), "{text}");
        assert!(text.contains("a.added") && text.contains("(not in the baseline)"), "{text}");
        assert!(!text.contains("a.gone"), "benchmarks that did not run are left out");
    }

    #[test]
    fn baseline_names_are_file_names() {
        for good in ["main", "v1.2", "release_3", "a-b"] {
            validate_baseline_name(good).unwrap();
        }
        for bad in ["", "..", ".hidden", "a/b", "a\\b", "with space", "ü"] {
            assert!(validate_baseline_name(bad).is_err(), "{bad}");
        }
    }
}
