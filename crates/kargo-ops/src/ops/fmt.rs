//! `kargo fmt`: formats Kotlin sources with ktlint.
//!
//! Like Dokka for `kargo doc`, ktlint is fetched from Maven Central (its
//! self-contained `ktlint-cli` jar) and run on the project's JDK, so nothing has
//! to be on `PATH`. ktlint reads its rules from `.editorconfig` files itself.
//!
//! The files are every `.kt` and `.kts` file under `src/`, `src-<target>/`,
//! `tests/` and `tests-<target>/`, minus `[fmt] exclude`. They are handed to
//! ktlint as globs on stdin, which keeps the command line short on Windows.
//!
//! [`format_text`] formats one document's text instead (ktlint's `--stdin` mode), for the
//! language server's `textDocument/formatting`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, bail};

use super::context::{BuildContext, MIN_JDK, fat_jar, jdk_needs};
use crate::workspace::Project;
use kargo_manifest::FmtSettings;
use kargo_maven::MavenClient;
use kargo_toolchain::Jdk;
use kargo_util::config::Config;
use kargo_util::fs::collect_files;
use kargo_util::process;
use kargo_util::shell::Verbosity;

/// ktlint version used unless `[fmt] ktlint` overrides it.
pub const KTLINT_VERSION: &str = "1.8.0";

const KTLINT_GROUP: &str = "com.pinterest.ktlint";

/// File extensions `kargo fmt` and `kargo lint` look at.
pub(crate) const KOTLIN_EXTENSIONS: [&str; 2] = ["kt", "kts"];

#[derive(Debug, Clone, Default)]
pub struct FmtOptions {
    /// Only report unformatted code (`--check`) instead of rewriting files.
    pub check: bool,
}

/// `kargo fetch --tools`: downloads the ktlint jar that `kargo fmt` runs.
pub fn fetch_tool(ctx: &BuildContext) -> Result<()> {
    let version = ctx.ws.manifest().fmt.ktlint_version.as_deref().unwrap_or(KTLINT_VERSION);
    ctx.tool_fat_jar(KTLINT_GROUP, "ktlint-cli", version).with_context(|| format!("failed to fetch ktlint {version}"))?;
    Ok(())
}

/// Runs ktlint. Returns `false` if it found problems: any in `--check` mode,
/// otherwise the ones it could not fix.
pub fn fmt(ctx: &BuildContext, opts: &FmtOptions) -> Result<bool> {
    let start = Instant::now();
    let ws = ctx.ws;
    let roots = kotlin_roots(ctx)?;
    let settings = &ws.manifest().fmt;
    let version = settings.ktlint_version.as_deref().unwrap_or(KTLINT_VERSION);
    let jdk = ctx.jdk()?;
    let jar = ctx.tool_fat_jar(KTLINT_GROUP, "ktlint-cli", version).with_context(|| format!("failed to fetch ktlint {version}"))?;

    let pkg = &ws.manifest().package;
    if opts.check {
        ctx.shell().status("Checking", format!("formatting of {} v{} (ktlint {version})", pkg.name, pkg.version));
    } else {
        ctx.shell().status("Formatting", format!("{} v{} (ktlint {version})", pkg.name, pkg.version));
    }
    let mut cmd = ktlint_command(jdk, &jar);
    cmd.current_dir(ws.root()).args(ktlint_args(opts.check, ctx.shell().verbosity(), settings));
    cmd.stdin(Stdio::piped());
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let mut child = cmd.spawn().with_context(|| format!("failed to start ktlint (`{}`)", cmd.get_program().to_string_lossy()))?;
    {
        let mut stdin = child.stdin.take().context("failed to open ktlint's stdin")?;
        stdin.write_all(ktlint_patterns(&roots, settings).join("\0").as_bytes()).context("failed to pass the source globs to ktlint")?;
    }
    let status = child.wait().context("failed to wait for ktlint")?;
    match status.code() {
        Some(0) => {
            ctx.shell().status("Finished", format!("ktlint in {:.2}s", start.elapsed().as_secs_f64()));
            Ok(true)
        }
        Some(1) => Ok(false),
        _ => bail!("ktlint failed ({}); see its output above", process::describe_status(status)),
    }
}

/// `java -jar ktlint`, with the flags the JDK needs.
fn ktlint_command(jdk: &Jdk, jar: &Path) -> Command {
    let mut cmd = Command::new(jdk.java());
    if jdk.version >= 23 {
        // ktlint bundles IntelliJ code that uses sun.misc.Unsafe, which JDK 23+ warns about on every run.
        cmd.arg("--sun-misc-unsafe-memory-access=allow");
    }
    cmd.arg("-jar").arg(jar);
    cmd
}

/// Formats the text of the Kotlin file `file` of `package` like `kargo fmt` would: with the same
/// ktlint, on the package's JDK, reading the rules from the `.editorconfig` files above `file`.
/// The text goes through ktlint's `--stdin` mode, so the file on disk is not read or changed.
///
/// Returns `None` for a file `[fmt] exclude` leaves out. Unlike [`fmt`], this takes no lock on
/// `target/` and prints nothing, so it can run while the project builds. Downloading ktlint and
/// the JDK follows `--offline` like `kargo fmt`.
pub fn format_text(config: &Config, project: &Project, package: usize, file: &Path, text: &str) -> Result<Option<String>> {
    let ws = project.package(package);
    let settings = &ws.manifest().fmt;
    if excluded(ws.root(), file, settings)? {
        return Ok(None);
    }
    let version = settings.ktlint_version.as_deref().unwrap_or(KTLINT_VERSION);
    let jdk = kargo_toolchain::jdks::resolve_for(config, &ws.manifest().toolchain.jdk, jdk_needs(ws.manifest()))?;
    jdk.require_version(MIN_JDK)?;
    let client = MavenClient::new(config, project.repositories());
    let jar = fat_jar(&client, KTLINT_GROUP, "ktlint-cli", version).with_context(|| format!("failed to fetch ktlint {version}"))?;

    let mut cmd = ktlint_command(&jdk, &jar);
    // No log at all: ktlint writes it to stdout, where only the formatted text may be.
    cmd.current_dir(ws.root()).args(["--format", "--stdin", "--log-level=none"]).args(code_style_arg(settings));
    // The virtual location of the text: where ktlint looks for `.editorconfig` files.
    cmd.arg(format!("--stdin-path={}", file.display()));
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("failed to start ktlint (`{}`)", cmd.get_program().to_string_lossy()))?;
    let mut stdin = child.stdin.take().context("failed to open ktlint's stdin")?;
    // On a thread, so a long file can't fill ktlint's output pipe while we are still writing.
    let input = text.to_owned();
    let writer = thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().context("failed to wait for ktlint")?;
    writer.join().map_err(|_| anyhow::anyhow!("failed to pass the text to ktlint"))?.context("failed to pass the text to ktlint")?;
    // Exit code 1 means violations ktlint can't fix remain; the text is still formatted.
    // No text at all means it could not read the file, a syntax error for one.
    let formatted = String::from_utf8(output.stdout).context("ktlint printed text that is not UTF-8")?;
    if formatted.is_empty() && !text.trim().is_empty() {
        bail!("ktlint could not format {}: {}", file.display(), ktlint_problem(&String::from_utf8_lossy(&output.stderr)));
    }
    Ok(Some(formatted))
}

/// Whether `[fmt] exclude` leaves out `file`. The patterns are relative to the package root and
/// use `.gitignore` syntax, as ktlint reads them.
fn excluded(root: &Path, file: &Path, settings: &FmtSettings) -> Result<bool> {
    if settings.exclude.is_empty() {
        return Ok(false);
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    for pattern in &settings.exclude {
        builder.add_line(None, pattern).with_context(|| format!("invalid [fmt] exclude pattern `{pattern}`"))?;
    }
    let matcher = builder.build().context("failed to read the [fmt] exclude patterns")?;
    let Ok(relative) = file.strip_prefix(root) else { return Ok(false) };
    Ok(matcher.matched_path_or_any_parents(relative, false).is_ignore())
}

/// Why ktlint printed no text: its stderr without the JVM's warnings and its summary.
fn ktlint_problem(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .take_while(|l| !l.starts_with("Summary error count"))
        .filter(|l| !l.is_empty() && !l.starts_with("WARNING:"))
        .collect();
    // For a syntax error the log line only says "Can not parse input"; the next one has the position.
    lines.iter().find(|l| l.contains("Not a valid Kotlin file")).map_or_else(|| lines.join("; "), |l| (*l).to_owned())
}

/// The existing source roots, failing if none of them holds a Kotlin file.
pub(crate) fn kotlin_roots(ctx: &BuildContext) -> Result<Vec<PathBuf>> {
    let ws = ctx.ws;
    let roots = ws.source_roots();
    if roots.iter().all(|r| collect_files(&ws.root().join(r), &KOTLIN_EXTENSIONS).is_empty()) {
        bail!("no Kotlin sources found; add them under `src/`, `src-<target>/`, `tests/` or `tests-<target>/`");
    }
    Ok(roots)
}

fn ktlint_args(check: bool, verbosity: Verbosity, settings: &FmtSettings) -> Vec<String> {
    let mut args = vec!["--relative".to_owned(), "--patterns-from-stdin".to_owned()];
    args.extend(code_style_arg(settings));
    if !check {
        args.push("--format".to_owned());
    }
    // Kargo prints its own hint on failure, so ktlint's "use `ktlint --format`" warning is noise.
    args.push(if verbosity == Verbosity::Verbose { "--log-level=info" } else { "--log-level=error" }.to_owned());
    args
}

/// `--code-style=<style>` when `[fmt] code-style` is set.
fn code_style_arg(settings: &FmtSettings) -> Option<String> {
    settings.code_style.as_ref().map(|style| format!("--code-style={style}"))
}

/// ktlint globs, relative to the project root: every Kotlin file of each root, then the exclusions.
fn ktlint_patterns(roots: &[PathBuf], settings: &FmtSettings) -> Vec<String> {
    let mut patterns: Vec<String> = Vec::new();
    for root in roots {
        let root = root.to_string_lossy().replace('\\', "/");
        patterns.extend(KOTLIN_EXTENSIONS.iter().map(|ext| format!("{root}/**/*.{ext}")));
    }
    patterns.extend(settings.exclude.iter().map(|p| format!("!{p}")));
    patterns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ktlint_patterns() {
        let settings = FmtSettings { ktlint_version: None, code_style: None, exclude: vec!["src/generated/**".to_owned()] };
        let roots = [PathBuf::from("src"), PathBuf::from("tests-jvm")];
        assert_eq!(
            ktlint_patterns(&roots, &settings),
            ["src/**/*.kt", "src/**/*.kts", "tests-jvm/**/*.kt", "tests-jvm/**/*.kts", "!src/generated/**"]
        );
    }

    #[test]
    fn exclude_patterns_match_like_ktlints() {
        let settings = FmtSettings {
            ktlint_version: None,
            code_style: None,
            exclude: vec!["src/generated/**".to_owned(), "**/*Generated.kt".to_owned()],
        };
        let root = Path::new("/p");
        assert!(excluded(root, Path::new("/p/src/generated/A.kt"), &settings).unwrap());
        assert!(excluded(root, Path::new("/p/src/UserGenerated.kt"), &settings).unwrap());
        assert!(!excluded(root, Path::new("/p/src/Main.kt"), &settings).unwrap());
        assert!(!excluded(root, Path::new("/elsewhere/src/generated/A.kt"), &settings).unwrap());
        assert!(!excluded(root, Path::new("/p/src/generated/A.kt"), &FmtSettings::default()).unwrap());
    }

    #[test]
    fn reports_why_ktlint_gave_no_text() {
        let stderr = "WARNING: restricted\n<stdin>:1:7: Not a valid Kotlin file (1:7 expecting ')') ()\n\nSummary error count (descending) by rule:\n  Not a valid Kotlin file: 1\n";
        assert_eq!(ktlint_problem(stderr), "<stdin>:1:7: Not a valid Kotlin file (1:7 expecting ')') ()");
        assert_eq!(ktlint_problem("boom\nWARNING: x"), "boom");
    }

    #[test]
    fn check_mode_does_not_rewrite_files() {
        assert!(ktlint_args(false, Verbosity::Normal, &FmtSettings::default()).contains(&"--format".to_owned()));
        assert!(!ktlint_args(true, Verbosity::Normal, &FmtSettings::default()).contains(&"--format".to_owned()));
        assert!(ktlint_args(true, Verbosity::Normal, &FmtSettings::default()).contains(&"--log-level=error".to_owned()));
        assert!(ktlint_args(true, Verbosity::Verbose, &FmtSettings::default()).contains(&"--log-level=info".to_owned()));
    }

    #[test]
    fn passes_the_code_style() {
        let settings = FmtSettings { code_style: Some("intellij_idea".to_owned()), ..FmtSettings::default() };
        assert!(ktlint_args(false, Verbosity::Normal, &settings).contains(&"--code-style=intellij_idea".to_owned()));
        assert_eq!(code_style_arg(&FmtSettings::default()), None);
    }
}
