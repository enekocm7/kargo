//! The JUnit Platform listener behind `kargo test --list` and the per-test JSON
//! messages of `kargo test --message-format json` on the JVM (see
//! `junit/KargoTestListener.java`). It is compiled once per JDK and launcher and
//! put on the console launcher's `--class-path`, which registers it through
//! `META-INF/services`. It prints one JSON object per line, after [`EVENT_PREFIX`].

use std::path::{Path, PathBuf};

use anyhow::Result;

use kargo_toolchain::Jdk;
use kargo_util::hash::{Fingerprint, sha256_bytes};

const SOURCE: &str = include_str!("junit/KargoTestListener.java");

/// What starts each line the listener prints.
pub const EVENT_PREFIX: &str = "##kargo ";

/// The system property that makes the listener print the test plan instead of results.
pub const LIST_PROPERTY: &str = "kargo.test.list";

/// The directory holding the compiled listener, compiling it under `cache` if needed.
pub fn listener(cache: &Path, jdk: &Jdk, launcher: &Path) -> Result<PathBuf> {
    let mut fp = Fingerprint::new();
    fp.str(&sha256_bytes(SOURCE.as_bytes())).str(&launcher.to_string_lossy()).str(&jdk.version.to_string());
    let classes = cache.join(format!("listener-{}", &fp.finish()[..16]));
    if !classes.join("KargoTestListener.class").is_file() {
        let services = [("META-INF/services/org.junit.platform.launcher.TestExecutionListener", "KargoTestListener\n")];
        crate::daemon::compile_java(cache, jdk, "KargoTestListener.java", SOURCE, &[launcher.to_path_buf()], &services, &classes)?;
    }
    Ok(classes)
}

/// The JSON message on a line the listener printed, if it is one.
pub fn parse_event(line: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let json = line.trim_end().strip_prefix(EVENT_PREFIX)?;
    match serde_json::from_str(json) {
        Ok(serde_json::Value::Object(map)) => Some(map),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events() {
        let event = parse_event(r#"##kargo {"reason":"test-started","id":"[engine:junit-jupiter]","name":"a\"b"}"#).unwrap();
        assert_eq!(event["reason"], "test-started");
        assert_eq!(event["name"], "a\"b");
        assert!(parse_event("plain output").is_none());
        assert!(parse_event("##kargo not json").is_none());
        assert!(parse_event("##kargo [1]").is_none());
    }
}
