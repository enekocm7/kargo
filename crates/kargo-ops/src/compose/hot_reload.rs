//! Compose Hot Reload for `kargo run --hot-reload`: the program keeps running across rebuilds, and the
//! Java agent of `KargoHotReload.java` swaps the classes that changed and recomposes the UI with them.
//!
//! Kargo and the agent talk through files in the state directory (`target/<profile>/jvm/hot-reload`):
//! after a build, Kargo writes a new request number to `request` and waits for the agent's answer in
//! `result`. An answer of `restart`, no answer, or a different command line (new dependencies, new JVM
//! options) makes Kargo start the program again instead.
//!
//! The program runs on JetBrains Runtime with `-XX:+AllowEnhancedClassRedefinition` when Kargo can get
//! one, so adding a function or a field reloads too. On another JDK only changed method bodies do.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use kargo_toolchain::Jdk;
use kargo_util::config::Config;

const SOURCE: &str = include_str!("KargoHotReload.java");
const AGENT_CLASS: &str = "kargo.hotreload.KargoHotReload";

/// How long Kargo waits for the agent to answer before it restarts the program.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);

/// The state directory of a program started with the agent, and the command line it was started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotReload {
    pub state: PathBuf,
    /// The command line without the program's arguments; a new one needs a restart.
    pub command: String,
}

/// What the agent did with a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// It redefined this many classes and recomposed.
    Reloaded(usize),
    /// The change cannot be applied to the running program, for this reason.
    Restart(String),
}

impl HotReload {
    /// Prepares `state` for a new program that watches `class_dirs`, and returns the JVM options that
    /// load the agent.
    pub fn agent_args(config: &Config, jdk: &Jdk, state: &Path, class_dirs: &[PathBuf]) -> Result<Vec<String>> {
        std::fs::create_dir_all(state).with_context(|| format!("failed to create {}", state.display()))?;
        let dirs: String = class_dirs.iter().map(|d| format!("{}\n", d.display())).collect();
        std::fs::write(state.join("classes"), dirs)?;
        // `request` keeps counting: this runs on every build, also when the running program is then
        // asked to reload, and an agent only answers a number it has not seen. A new agent starts from
        // whatever number is there.
        let jar = agent_jar(config, jdk)?;
        Ok(vec![format!("-javaagent:{}={}", jar.display(), state.display())])
    }

    /// Asks the running agent to reload, and waits for its answer.
    pub fn request(&self) -> Answer {
        let request = std::fs::read_to_string(self.state.join("request")).ok().and_then(|n| n.trim().parse::<u64>().ok()).unwrap_or(0) + 1;
        if let Err(e) = std::fs::write(self.state.join("request"), request.to_string()) {
            return Answer::Restart(format!("cannot write the reload request: {e}"));
        }
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(answer) = std::fs::read_to_string(self.state.join("result")).ok().and_then(|text| parse_answer(&text, request)) {
                return answer;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Answer::Restart("the program did not answer the reload request".to_owned())
    }
}

/// The copy of the classes the app runs from, in the state directory.
pub fn live_classes(state: &Path) -> PathBuf {
    state.join("live-classes")
}

/// Brings the [`live_classes`] in `state` up to date with the compiler's `classes`: changed and new
/// files are copied, deleted ones removed, and unchanged ones left alone, so the agent sees only what
/// the build changed. Returns the directory.
pub fn sync_classes(classes: &Path, state: &Path) -> Result<PathBuf> {
    let live = live_classes(state);
    let wanted = crate::wasm_output::all_files(classes);
    for file in &wanted {
        let relative = file.strip_prefix(classes).unwrap_or(file);
        let dest = live.join(relative);
        let bytes = std::fs::read(file).with_context(|| format!("failed to read {}", file.display()))?;
        if std::fs::read(&dest).is_ok_and(|old| old == bytes) {
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, bytes).with_context(|| format!("failed to write {}", dest.display()))?;
    }
    for old in crate::wasm_output::all_files(&live) {
        let relative = old.strip_prefix(&live).unwrap_or(&old);
        if !classes.join(relative).is_file() {
            let _ = std::fs::remove_file(&old);
        }
    }
    std::fs::create_dir_all(&live)?;
    Ok(live)
}

/// The agent's answer to `request` in the text of `result`, if it is there yet.
fn parse_answer(text: &str, request: u64) -> Option<Answer> {
    let mut parts = text.trim().splitn(3, ' ');
    if parts.next()?.parse::<u64>().ok()? != request {
        return None;
    }
    match parts.next()? {
        "ok" => Some(Answer::Reloaded(parts.next().and_then(|n| n.trim().parse().ok()).unwrap_or(0))),
        _ => Some(Answer::Restart(parts.next().unwrap_or("the change cannot be applied in place").trim().to_owned())),
    }
}

/// The agent jar, built with `jdk`'s `javac` the first time, under `~/.kargo/hot-reload/<hash>/`.
fn agent_jar(config: &Config, jdk: &Jdk) -> Result<PathBuf> {
    let key = kargo_util::hash::sha256_bytes(format!("{SOURCE}\n{}", jdk.version).as_bytes());
    let dir = config.home().join("hot-reload").join(&key[..16]);
    let jar = dir.join("kargo-hot-reload.jar");
    if jar.is_file() {
        return Ok(jar);
    }
    if !jdk.has_javac() {
        bail!("{} has no `javac` to build the hot reload agent; use a full JDK", jdk.home.display());
    }
    let build = dir.join("build");
    kargo_util::fs::remove_dir_all_if_exists(&build)?;
    std::fs::create_dir_all(&build)?;
    let source = build.join("KargoHotReload.java");
    std::fs::write(&source, SOURCE)?;
    let classes = build.join("classes");
    let output = Command::new(jdk.javac())
        .arg("-d")
        .arg(&classes)
        .arg(&source)
        .stdin(Stdio::null())
        .output()
        .context("failed to run javac for the hot reload agent")?;
    if !output.status.success() {
        bail!("javac failed to build the hot reload agent: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let temp = dir.join("kargo-hot-reload.jar.tmp");
    write_jar(&classes, &temp)?;
    kargo_util::fs::rename_with_retries(&temp, &jar)?;
    let _ = kargo_util::fs::remove_dir_all_if_exists(&build);
    Ok(jar)
}

/// Writes the agent's classes into a jar whose manifest names its `premain` class.
fn write_jar(classes: &Path, jar: &Path) -> Result<()> {
    let file = std::fs::File::create(jar).with_context(|| format!("failed to create {}", jar.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("META-INF/MANIFEST.MF", options)?;
    write!(zip, "Manifest-Version: 1.0\r\nPremain-Class: {AGENT_CLASS}\r\nCan-Redefine-Classes: true\r\n\r\n")?;
    for class in kargo_util::fs::collect_files(classes, &["class"]) {
        let name = kargo_util::process::slash(class.strip_prefix(classes).unwrap_or(&class));
        zip.start_file(name, options)?;
        zip.write_all(&std::fs::read(&class)?)?;
    }
    zip.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers() {
        assert_eq!(parse_answer("3 ok 5\n", 3), Some(Answer::Reloaded(5)));
        assert_eq!(parse_answer("2 ok 5", 3), None, "an answer to an earlier request is not this one's");
        assert_eq!(
            parse_answer("4 restart UnsupportedOperationException: class redefinition failed: attempted to add a method", 4),
            Some(Answer::Restart("UnsupportedOperationException: class redefinition failed: attempted to add a method".to_owned()))
        );
        assert_eq!(parse_answer("", 1), None);
    }

    #[test]
    fn the_live_classes_follow_the_build() {
        let dir = tempfile::tempdir().unwrap();
        let (classes, state) = (dir.path().join("classes"), dir.path().join("state"));
        std::fs::create_dir_all(classes.join("a")).unwrap();
        std::fs::write(classes.join("a/A.class"), "a1").unwrap();
        std::fs::write(classes.join("B.class"), "b1").unwrap();
        let live = sync_classes(&classes, &state).unwrap();
        assert_eq!(std::fs::read_to_string(live.join("a/A.class")).unwrap(), "a1");

        let untouched = std::fs::metadata(live.join("B.class")).unwrap().modified().unwrap();
        std::fs::write(classes.join("B.class"), "b1").unwrap();
        std::fs::remove_file(classes.join("a/A.class")).unwrap();
        std::fs::write(classes.join("C.class"), "c1").unwrap();
        sync_classes(&classes, &state).unwrap();
        assert!(!live.join("a/A.class").exists(), "a deleted class goes");
        assert_eq!(std::fs::read_to_string(live.join("C.class")).unwrap(), "c1");
        assert_eq!(std::fs::metadata(live.join("B.class")).unwrap().modified().unwrap(), untouched, "an unchanged class is not rewritten");
    }

    #[test]
    fn requests_count_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("request"), "7").unwrap();
        std::fs::write(dir.path().join("result"), "8 ok 2").unwrap();
        let hot = HotReload { state: dir.path().to_path_buf(), command: String::new() };
        assert_eq!(hot.request(), Answer::Reloaded(2));
        assert_eq!(std::fs::read_to_string(dir.path().join("request")).unwrap(), "8");
    }
}
