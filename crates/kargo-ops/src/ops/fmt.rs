//! `kargo fmt`: formats Kotlin sources with ktlint.
//!
//! Like Dokka for `kargo doc`, ktlint is fetched from Maven Central (its
//! self-contained `ktlint-cli` jar) and run on the project's JDK, so nothing has
//! to be on `PATH`. ktlint reads its rules from `.editorconfig` files itself.
//!
//! The files are every `.kt` and `.kts` file under `src/`, `src-<target>/`,
//! `tests/` and `tests-<target>/`, minus `[fmt] exclude`. They are handed to
//! ktlint as globs on stdin, which keeps the command line short on Windows.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use super::context::BuildContext;
use kargo_manifest::FmtSettings;
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
    let mut cmd = Command::new(jdk.java());
    if jdk.version >= 23 {
        // ktlint bundles IntelliJ code that uses sun.misc.Unsafe, which JDK 23+ warns about on every run.
        cmd.arg("--sun-misc-unsafe-memory-access=allow");
    }
    cmd.current_dir(ws.root()).arg("-jar").arg(&jar).args(ktlint_args(opts.check, ctx.shell().verbosity()));
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

/// The existing source roots, failing if none of them holds a Kotlin file.
pub(crate) fn kotlin_roots(ctx: &BuildContext) -> Result<Vec<PathBuf>> {
    let ws = ctx.ws;
    let roots = ws.source_roots();
    if roots.iter().all(|r| collect_files(&ws.root().join(r), &KOTLIN_EXTENSIONS).is_empty()) {
        bail!("no Kotlin sources found; add them under `src/`, `src-<target>/`, `tests/` or `tests-<target>/`");
    }
    Ok(roots)
}

fn ktlint_args(check: bool, verbosity: Verbosity) -> Vec<String> {
    let mut args = vec!["--relative".to_owned(), "--patterns-from-stdin".to_owned()];
    if !check {
        args.push("--format".to_owned());
    }
    // Kargo prints its own hint on failure, so ktlint's "use `ktlint --format`" warning is noise.
    args.push(if verbosity == Verbosity::Verbose { "--log-level=info" } else { "--log-level=error" }.to_owned());
    args
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
        let settings = FmtSettings { ktlint_version: None, exclude: vec!["src/generated/**".to_owned()] };
        let roots = [PathBuf::from("src"), PathBuf::from("tests-jvm")];
        assert_eq!(
            ktlint_patterns(&roots, &settings),
            ["src/**/*.kt", "src/**/*.kts", "tests-jvm/**/*.kt", "tests-jvm/**/*.kts", "!src/generated/**"]
        );
    }

    #[test]
    fn check_mode_does_not_rewrite_files() {
        assert!(ktlint_args(false, Verbosity::Normal).contains(&"--format".to_owned()));
        assert!(!ktlint_args(true, Verbosity::Normal).contains(&"--format".to_owned()));
        assert!(ktlint_args(true, Verbosity::Normal).contains(&"--log-level=error".to_owned()));
        assert!(ktlint_args(true, Verbosity::Verbose).contains(&"--log-level=info".to_owned()));
    }
}
