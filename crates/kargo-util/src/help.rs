//! Advice for errors: the closest known name for a typo ("did you mean ..."), errors that carry a `help:` line and
//! suggestions, and the links from error messages to the FAQ.

use std::fmt;

/// Where the documentation lives.
pub const SITE: &str = "https://enekocm7.github.io/kargo";

/// An error with advice for the user: what to do next (`help`) and the names that would fix it (`suggestions`).
/// `kargo` prints the help under the error, and `--message-format json` carries both fields.
#[derive(Debug)]
pub struct Helpful {
    message: String,
    help: Option<String>,
    suggestions: Vec<String>,
}

impl Helpful {
    /// The line printed as `help: ...`.
    pub fn help(&self) -> Option<&str> {
        self.help.as_deref()
    }

    /// The names that would fix the error, best first.
    pub fn suggestions(&self) -> &[String] {
        &self.suggestions
    }
}

impl fmt::Display for Helpful {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Helpful {}

/// An error whose `message` comes with a `help` line (and the `suggestions` it is about).
pub fn helpful(message: impl Into<String>, help: impl Into<String>, suggestions: Vec<String>) -> anyhow::Error {
    anyhow::Error::new(Helpful { message: message.into(), help: Some(help.into()), suggestions })
}

/// The advice in `err`'s chain, if any of its causes carries some.
pub fn advice(err: &anyhow::Error) -> Option<&Helpful> {
    err.chain().find_map(|cause| cause.downcast_ref::<Helpful>())
}

/// Every line of help for `err`: the advice it carries, then the FAQ entry that explains it.
pub fn help_lines(err: &anyhow::Error) -> Vec<String> {
    let mut lines: Vec<String> = advice(err).and_then(Helpful::help).map(str::to_owned).into_iter().collect();
    let text = err.chain().map(ToString::to_string).collect::<Vec<_>>().join(": ");
    if let Some(url) = faq_url(&text) {
        lines.push(format!("see {url}"));
    }
    lines
}

/// The candidate closest to `name` when it is near enough to be a typo (as Cargo suggests commands):
/// at most a third of the name's length (at least one) away, counting a swap of neighbors as one edit.
pub fn closest<'a, S: AsRef<str>>(name: &str, candidates: &'a [S]) -> Option<&'a str> {
    nearest(name, candidates, 1).into_iter().next()
}

/// Up to `limit` candidates near enough to `name` to be a typo, the nearest first.
pub fn nearest<'a, S: AsRef<str>>(name: &str, candidates: &'a [S], limit: usize) -> Vec<&'a str> {
    let max = name.chars().count().max(3) / 3;
    let name = name.to_ascii_lowercase();
    let mut found: Vec<(usize, &str)> = candidates
        .iter()
        .map(AsRef::as_ref)
        .map(|c| (edit_distance(&name, &c.to_ascii_lowercase()), c))
        .filter(|(d, _)| *d <= max)
        .collect();
    found.sort_by_key(|(d, _)| *d);
    found.dedup_by_key(|(_, c)| *c);
    // Only the names as near as the best one: the others are not what was meant.
    let best = found.first().map_or(0, |(d, _)| *d);
    found.into_iter().take_while(|(d, _)| *d <= best).take(limit).map(|(_, c)| c).collect()
}

/// Optimal string alignment distance: insertions, deletions, substitutions and swaps of neighbors. Hyphens and
/// underscores count as the same character, so `linux_x64` is no edit away from `linux-x64`.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let same = |x: char, y: char| x == y || (matches!(x, '-' | '_') && matches!(y, '-' | '_'));
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut rows = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in rows[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(!same(a[i - 1], b[j - 1]));
            let mut best = (rows[i - 1][j] + 1).min(rows[i][j - 1] + 1).min(rows[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && same(a[i - 1], b[j - 2]) && same(a[i - 2], b[j - 1]) {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[a.len()][b.len()]
}

/// An error for an unknown name: `message` as given, with a "did you mean" help line when `known` has a close match.
pub fn unknown<S: AsRef<str>>(message: impl Into<String>, name: &str, known: &[S]) -> anyhow::Error {
    let suggestions: Vec<String> = nearest(name, known, 3).into_iter().map(str::to_owned).collect();
    match suggestions.first() {
        Some(close) => {
            let help = format!("did you mean `{close}`?");
            helpful(message, help, suggestions)
        }
        None => anyhow::anyhow!("{}", message.into()),
    }
}

/// The error messages the FAQ explains, with the anchor of the entry under `reference/faq/`.
/// `tests/faq.rs` checks every anchor against the headings of the page.
pub const FAQ: &[(&str, &str)] = &[
    ("no Java installation found", "no-java-installation-found-on-java_home-or-path-and-"),
    ("cannot run on JDK", "kotlin--cannot-run-on-jdk-"),
    ("is newer than JDK", "jvm-target--is-newer-than-jdk--or-invalid-targetjvm-jvm-target"),
    ("invalid [target.jvm] jvm-target", "jvm-target--is-newer-than-jdk--or-invalid-targetjvm-jvm-target"),
    ("could not determine the main class", "could-not-determine-the-main-class"),
    ("Node.js is required to run", "nodejs-is-required-to-run-kotlinwasm-programs"),
    ("no browser found to run", "no-browser-found-to-run-the-wasm-tests"),
    ("Android SDK not found", "android-sdk-not-found-set-android_home"),
    ("there is no terminal to choose on", "more-than-one-device-is-connected-and-there-is-no-terminal-to-choose-on"),
    ("no device or emulator is connected", "no-device-or-emulator-is-connected"),
    ("is unauthorized", "r58m123abc-is-unauthorized"),
    ("is too old for Android Gradle Plugin", "gradle--is-too-old-for-android-gradle-plugin-"),
    ("is not installed and Kargo is offline", "gradle--is-not-installed-and-kargo-is-offline"),
    ("is not in the local cache and Kargo is running in offline mode", "-is-not-in-the-local-cache-and-kargo-is-running-in-offline-mode"),
    ("but --locked was passed", "kargolock-needs-to-be-updated--but---locked-was-passed"),
    ("the native target has no build for this host", "the-native-target-has-no-build-for-this-host"),
    ("checksum mismatch", "checksum-mismatch-for-a-locked-artifact"),
];

/// The FAQ page's URL for the first entry of [`FAQ`] whose message appears in `text`.
pub fn faq_url(text: &str) -> Option<String> {
    FAQ.iter().find(|(message, _)| text.contains(message)).map(|(_, anchor)| format!("{SITE}/reference/faq/#{anchor}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_the_closest_name() {
        let known = ["build", "bench", "run", "test", "tree", "check"];
        assert_eq!(closest("biuld", &known), Some("build"));
        assert_eq!(closest("chek", &known), Some("check"));
        assert_eq!(closest("tset", &known), Some("test"));
        assert_eq!(closest("definitely-not-a-command", &known), None);
        assert_eq!(closest("x", &known), None);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn hyphens_and_underscores_and_case_are_not_typos() {
        let known = ["linux-x64", "macos-arm64"];
        assert_eq!(closest("linux_x64", &known), Some("linux-x64"));
        assert_eq!(closest("linuxX64", &known), Some("linux-x64"));
        assert_eq!(closest("macosArm64", &["macos-arm64"]), Some("macos-arm64"));
    }

    #[test]
    fn nearest_lists_the_best_first() {
        let known = ["serialization", "coroutines", "compose", "ktor"];
        assert_eq!(nearest("serialisation", &known, 3), vec!["serialization"]);
        assert_eq!(nearest("coroutine", &known, 3), vec!["coroutines"]);
    }

    #[test]
    fn helpful_errors_are_found_through_context() {
        use anyhow::Context;
        let err = helpful("no such thing", "did you mean `x`?", vec!["x".into()]);
        let err = Err::<(), _>(err).context("while doing it").unwrap_err();
        let advice = advice(&err).unwrap();
        assert_eq!(advice.help(), Some("did you mean `x`?"));
        assert_eq!(advice.suggestions(), ["x"]);
        assert_eq!(help_lines(&err), vec!["did you mean `x`?".to_owned()]);
    }

    #[test]
    fn errors_with_a_faq_entry_link_to_it() {
        let err = anyhow::anyhow!("kargo.lock needs to be updated for `jvm/test` but --locked was passed; run `kargo update`");
        let lines = help_lines(&err);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("see https://enekocm7.github.io/kargo/reference/faq/#"), "{lines:?}");
        assert!(help_lines(&anyhow::anyhow!("something else")).is_empty());
    }
}
