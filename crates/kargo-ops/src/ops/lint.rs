//! `kargo lint`: static analysis of Kotlin sources with detekt.
//!
//! detekt's self-contained `detekt-cli` jar is fetched from Maven Central and
//! run on the project's JDK. It analyses `src/`, `src-<target>/`, `tests/` and
//! `tests-<target>/`, starting from detekt's default rules.
//!
//! Configuration is layered: a small file Kargo writes first (it turns off
//! `InvalidPackageDeclaration`, because Kargo projects keep sources directly in
//! `src/` rather than in package directories, and it skips `UnusedPrivateProperty`
//! on `@JsModule` declarations), then the user's `detekt.yml` or
//! `[lint] config` files, which override it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use super::context::BuildContext;
use super::fmt::kotlin_roots;
use kargo_manifest::LintSettings;
use kargo_util::fs::write_string_atomic;
use kargo_util::process;

/// detekt version used unless `[lint] detekt` overrides it.
pub const DETEKT_VERSION: &str = "1.23.8";

const DETEKT_GROUP: &str = "io.gitlab.arturbosch.detekt";

/// Configuration file used when `[lint] config` is not set and the file exists.
pub const DEFAULT_CONFIG: &str = "detekt.yml";

/// Baseline file used when `[lint] baseline` is not set.
pub const DEFAULT_BASELINE: &str = "detekt-baseline.xml";

/// Kargo's own detekt settings, overridden by the user's configuration.
const KARGO_CONFIG: &str = "\
# Written by Kargo. Your detekt.yml (or [lint] config) is applied on top of this file.
naming:
  InvalidPackageDeclaration:
    # Kargo projects keep sources directly in src/, not in package directories.
    active: false
style:
  UnusedPrivateProperty:
    # The constructor parameters of a JavaScript module's class describe the JavaScript signature.
    ignoreAnnotated:
      - 'JsModule'
      - 'JsNonModule'
";

#[derive(Debug, Clone, Default)]
pub struct LintOptions {
    /// Write the current issues to the baseline file instead of reporting them.
    pub create_baseline: bool,
}

/// `kargo fetch --tools`: downloads the detekt jar that `kargo lint` runs.
pub fn fetch_tool(ctx: &BuildContext) -> Result<()> {
    let version = ctx.ws.manifest().lint.detekt_version.as_deref().unwrap_or(DETEKT_VERSION);
    ctx.tool_fat_jar(DETEKT_GROUP, "detekt-cli", version).with_context(|| format!("failed to fetch detekt {version}"))?;
    Ok(())
}

/// Runs detekt. Returns `false` if it reported issues.
pub fn lint(ctx: &BuildContext, opts: &LintOptions) -> Result<bool> {
    let start = Instant::now();
    let ws = ctx.ws;
    let root = ws.root();
    let roots = kotlin_roots(ctx)?;
    let settings = &ws.manifest().lint;
    let version = settings.detekt_version.as_deref().unwrap_or(DETEKT_VERSION);

    let user_configs = user_configs(root, settings)?;
    let baseline = settings.baseline.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_BASELINE));
    if !opts.create_baseline && settings.baseline.is_some() && !root.join(&baseline).is_file() {
        bail!("[lint] baseline file `{}` does not exist; create it with `kargo lint --create-baseline`", baseline.display());
    }
    let baseline = (opts.create_baseline || root.join(&baseline).is_file()).then_some(baseline);

    let jdk = ctx.jdk()?;
    let jar = ctx.tool_fat_jar(DETEKT_GROUP, "detekt-cli", version).with_context(|| format!("failed to fetch detekt {version}"))?;
    let work = ws.work_dir("lint");
    std::fs::create_dir_all(&work).with_context(|| format!("failed to create {}", work.display()))?;
    let kargo_config = work.join("kargo-detekt.yml");
    write_string_atomic(&kargo_config, KARGO_CONFIG)?;

    let mut configs = vec![kargo_config];
    configs.extend(user_configs);
    let args = DetektArgs { root, roots: &roots, configs: &configs, baseline: baseline.as_deref(), create_baseline: opts.create_baseline };

    let pkg = &ws.manifest().package;
    ctx.shell().status("Linting", format!("{} v{} (detekt {version})", pkg.name, pkg.version));
    let mut cmd = Command::new(jdk.java());
    if jdk.version >= 23 {
        // detekt bundles IntelliJ code that uses sun.misc.Unsafe, which JDK 23+ warns about on every run.
        cmd.arg("--sun-misc-unsafe-memory-access=allow");
    }
    cmd.current_dir(root).arg("-jar").arg(&jar).args(args.to_args());
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let output = cmd.output().with_context(|| format!("failed to start detekt (`{}`)", cmd.get_program().to_string_lossy()))?;
    // detekt prints absolute paths, while ktlint prints paths relative to the project root.
    let stdout = relativize(&String::from_utf8_lossy(&output.stdout), root);
    let stderr = relativize(&String::from_utf8_lossy(&output.stderr), root);
    let _ = std::io::stdout().write_all(stdout.as_bytes());
    let _ = std::io::stderr().write_all(stderr.as_bytes());
    let status = output.status;
    match status.code() {
        Some(0) => {
            if let (true, Some(baseline)) = (opts.create_baseline, &baseline) {
                ctx.shell().status("Created", format!("baseline {}", baseline.display()));
            }
            ctx.shell().status("Finished", format!("detekt in {:.2}s", start.elapsed().as_secs_f64()));
            Ok(true)
        }
        Some(2) => Ok(false),
        Some(3) => bail!("detekt rejected its configuration; fix the files it names above"),
        _ => bail!("detekt failed ({}); see its output above", process::describe_status(status)),
    }
}

/// Rewrites lines that start with a path under `root` to start with the path relative to it, with
/// `/` separators, as ktlint prints them. Other lines are left alone.
fn relativize(text: &str, root: &Path) -> String {
    let root = root.to_string_lossy();
    let root = root.strip_prefix(r"\\?\").unwrap_or(&root);
    let root = root.replace('\\', "/");
    let root = root.trim_end_matches('/');
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        match relative_line(line, root) {
            Some(line) => out.push_str(&line),
            None => out.push_str(line),
        }
    }
    out
}

fn relative_line(line: &str, root: &str) -> Option<String> {
    let line = line.strip_prefix(r"\\?\").unwrap_or(line);
    // The path ends at the first `:` that is not a drive letter's.
    let skip = if line.as_bytes().get(1) == Some(&b':') { 2 } else { 0 };
    let end = skip + line[skip..].find(':')?;
    let path = line[..end].replace('\\', "/");
    let rest = path.get(..root.len()).filter(|head| head.eq_ignore_ascii_case(root)).map(|_| &path[root.len()..])?;
    let rest = rest.strip_prefix('/')?;
    Some(format!("{rest}{}", &line[end..]))
}

/// The user's configuration files: `[lint] config`, else `detekt.yml` if it exists.
fn user_configs(root: &Path, settings: &LintSettings) -> Result<Vec<PathBuf>> {
    if settings.config.is_empty() {
        let default = PathBuf::from(DEFAULT_CONFIG);
        return Ok(if root.join(&default).is_file() { vec![default] } else { Vec::new() });
    }
    for config in &settings.config {
        if !root.join(config).is_file() {
            bail!("[lint] config file `{}` does not exist", config.display());
        }
    }
    Ok(settings.config.clone())
}

/// The detekt command line, with paths relative to the project root.
struct DetektArgs<'a> {
    root: &'a Path,
    roots: &'a [PathBuf],
    configs: &'a [PathBuf],
    baseline: Option<&'a Path>,
    create_baseline: bool,
}

impl DetektArgs<'_> {
    fn to_args(&self) -> Vec<String> {
        let join = |paths: &[PathBuf]| paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join(",");
        let mut args = vec![
            "--input".to_owned(),
            join(self.roots),
            "--base-path".to_owned(),
            self.root.to_string_lossy().into_owned(),
            "--build-upon-default-config".to_owned(),
            "--config".to_owned(),
            join(self.configs),
        ];
        if let Some(baseline) = self.baseline {
            args.extend(["--baseline".to_owned(), baseline.to_string_lossy().into_owned()]);
            if self.create_baseline {
                args.push("--create-baseline".to_owned());
            }
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kargo_config_skips_unused_private_property_on_js_modules() {
        assert!(KARGO_CONFIG.contains("UnusedPrivateProperty:"));
        assert!(KARGO_CONFIG.contains("- 'JsModule'"));
    }

    #[test]
    fn builds_detekt_arguments() {
        let roots = [PathBuf::from("src"), PathBuf::from("tests")];
        let configs = [PathBuf::from("/p/target/lint/kargo-detekt.yml"), PathBuf::from("detekt.yml")];
        let mut args = DetektArgs {
            root: Path::new("/p"),
            roots: &roots,
            configs: &configs,
            baseline: Some(Path::new("detekt-baseline.xml")),
            create_baseline: false,
        };
        let joined = args.to_args().join(" ");
        assert!(joined.starts_with("--input src,tests --base-path /p --build-upon-default-config"), "{joined}");
        assert!(joined.contains("--config /p/target/lint/kargo-detekt.yml,detekt.yml"), "{joined}");
        assert!(joined.ends_with("--baseline detekt-baseline.xml"), "{joined}");
        args.create_baseline = true;
        assert!(args.to_args().ends_with(&["--create-baseline".to_owned()]));
        args.baseline = None;
        assert!(!args.to_args().contains(&"--baseline".to_owned()));
    }

    #[test]
    fn prints_paths_relative_to_the_project() {
        let text = "Overall debt: 1min\n\nF:\\p\\tests\\GreetingTest.kt:9:1: Line too long [MaxLineLength]\n/p/src/Main.kt:3:4: x\n/other/Main.kt:1:1: y\n";
        assert_eq!(
            relativize(text, Path::new(r"F:\p")),
            "Overall debt: 1min\n\ntests/GreetingTest.kt:9:1: Line too long [MaxLineLength]\n/p/src/Main.kt:3:4: x\n/other/Main.kt:1:1: y\n"
        );
        assert_eq!(relativize("/p/src/Main.kt:3:4: x\n", Path::new("/p")), "src/Main.kt:3:4: x\n");
    }

    #[test]
    fn finds_user_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = LintSettings::default();
        assert!(user_configs(dir.path(), &settings).unwrap().is_empty());
        std::fs::write(dir.path().join(DEFAULT_CONFIG), "").unwrap();
        assert_eq!(user_configs(dir.path(), &settings).unwrap(), [PathBuf::from(DEFAULT_CONFIG)]);
        settings.config = vec![PathBuf::from("config/detekt.yml")];
        let err = user_configs(dir.path(), &settings).unwrap_err().to_string();
        assert!(err.contains("config/detekt.yml"), "{err}");
    }
}
