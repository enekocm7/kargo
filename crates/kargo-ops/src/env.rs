//! The project environment: `[env]` and `[profile.<name>.env]` from kargo.toml and the `.env`
//! files of a package, layered under the shell's environment. `kargo run`, `kargo test`, tasks
//! and hooks start their programs with it, and `[env] expose` compiles some of it into a
//! generated `BuildConfig` object.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use kargo_manifest::{EnvTable, EnvVar, is_env_name};

use crate::ops::new::{RESERVED_WORDS, package_segment};
use crate::workspace::Workspace;

/// The `.env` files of a package, in increasing priority: `.env`, `.env.<profile>`,
/// `.env.local` and `.env.<profile>.local`. A missing file is skipped.
pub fn standard_files(profile: &str) -> [String; 4] {
    [".env".to_owned(), format!(".env.{profile}"), ".env.local".to_owned(), format!(".env.{profile}.local")]
}

/// Which `.env` files a command reads (`--env-file`, `--no-env-file`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvOptions {
    /// Files named with `--env-file`. They are read last, so they win over every other file.
    pub files: Vec<PathBuf>,
    /// Read none of the `.env` files (`--no-env-file`); `[env]` still applies.
    pub no_files: bool,
}

/// Where the value of a variable came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The shell's environment.
    Shell,
    /// `[env]` or `[profile.<name>.env]`, named as it is written in kargo.toml.
    Manifest(String),
    /// A line of a `.env` file, named relative to the package root.
    File { file: String, line: usize },
}

impl Source {
    /// How the source is shown to the user: `shell`, `[env]`, or `.env.dev:3`.
    pub fn describe(&self) -> String {
        match self {
            Source::Shell => "shell".to_owned(),
            Source::Manifest(section) => section.clone(),
            Source::File { file, line } => format!("{file}:{line}"),
        }
    }
}

/// A resolved variable: its value and the source that won.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Var {
    pub value: String,
    pub source: Source,
}

/// A `.env` file that was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedFile {
    /// The file, relative to the package root when it is inside it.
    pub file: String,
    /// Keys with a non-empty value that look like secrets (see [`is_secret_name`]).
    pub secret_keys: Vec<String>,
}

/// The variables a program starts with, resolved from every source.
#[derive(Debug, Default)]
pub struct ProjectEnv {
    /// Every variable some source declares, with the value that wins.
    pub vars: BTreeMap<String, Var>,
    /// Required variables that no source sets, with the place that declares them.
    pub missing: Vec<(String, Source)>,
    /// The `.env` files that were read, in the order they override each other.
    pub files: Vec<LoadedFile>,
}

/// A `.env` file to read, and what asked for it. The standard files have no `asked_by`: they
/// may be absent, whereas a file named by kargo.toml or `--env-file` must exist.
struct FileRef {
    path: PathBuf,
    asked_by: Option<String>,
}

impl ProjectEnv {
    /// Resolves the environment of the package at `root` for `profile` (`dev` or `release`).
    ///
    /// From the lowest priority to the highest: `[env]`, `[profile.<profile>.env]`, the `.env`
    /// files in order (the standard ones, then `env-files` of both tables, then `--env-file`),
    /// then the shell. A variable with `force = true` in kargo.toml beats the shell and the files
    /// too. `shell` is the shell's environment.
    pub fn load(
        root: &Path,
        profile: &str,
        base: &EnvTable,
        profile_env: &EnvTable,
        opts: &EnvOptions,
        shell: &BTreeMap<String, String>,
    ) -> Result<Self> {
        let mut refs: Vec<FileRef> = Vec::new();
        if !opts.no_files {
            refs.extend(standard_files(profile).into_iter().map(|name| FileRef { path: root.join(name), asked_by: None }));
            let section = format!("[profile.{profile}.env]");
            for (table, by) in [(base, "[env]"), (profile_env, section.as_str())] {
                refs.extend(table.files.iter().map(|name| FileRef { path: root.join(name), asked_by: Some(format!("{by} env-files")) }));
            }
        }
        refs.extend(opts.files.iter().map(|path| FileRef { path: path.clone(), asked_by: Some("--env-file".to_owned()) }));

        let mut files: BTreeMap<String, Var> = BTreeMap::new();
        let mut loaded = Vec::new();
        for file_ref in refs {
            let text = match std::fs::read_to_string(&file_ref.path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => match &file_ref.asked_by {
                    None => continue,
                    Some(by) => bail!("{by} names `{}`, which does not exist", file_ref.path.display()),
                },
                Err(e) => return Err(e).with_context(|| format!("failed to read {}", file_ref.path.display())),
            };
            let file = file_ref.path.strip_prefix(root).unwrap_or(&file_ref.path).display().to_string();
            let known = |name: &str| files.get(name).map(|var| var.value.clone()).or_else(|| shell.get(name).cloned());
            let entries = parse_dotenv(&file, &text, &known)?;
            loaded.push(LoadedFile { file: file.clone(), secret_keys: secret_keys(&entries) });
            for (key, value, line) in entries {
                files.insert(key, Var { value, source: Source::File { file: file.clone(), line } });
            }
        }

        // `[profile.<name>.env]` replaces `[env]` variable by variable.
        let mut manifest: BTreeMap<&str, (&EnvVar, Source)> = BTreeMap::new();
        for (name, var) in &base.vars {
            manifest.insert(name, (var, Source::Manifest("[env]".to_owned())));
        }
        let section = format!("[profile.{profile}.env]");
        for (name, var) in &profile_env.vars {
            manifest.insert(name, (var, Source::Manifest(section.clone())));
        }

        // Exposed names are resolved too, so `kargo env` shows them even when only the shell sets them.
        let names: BTreeSet<&str> =
            manifest.keys().copied().chain(files.keys().map(String::as_str)).chain(base.expose.iter().map(String::as_str)).collect();
        let mut env = ProjectEnv { files: loaded, ..ProjectEnv::default() };
        for name in names {
            let declared = manifest.get(name);
            let forced = declared.filter(|(var, _)| var.force && var.value.is_some());
            let winner = if let Some((var, source)) = forced {
                Some(Var { value: manifest_value(var, root), source: source.clone() })
            } else if let Some(value) = shell.get(name) {
                Some(Var { value: value.clone(), source: Source::Shell })
            } else if let Some(var) = files.get(name) {
                Some(var.clone())
            } else if let Some((var, source)) = declared.filter(|(var, _)| var.value.is_some()) {
                Some(Var { value: manifest_value(var, root), source: source.clone() })
            } else {
                None
            };
            match (winner, declared) {
                (Some(var), _) => {
                    env.vars.insert(name.to_owned(), var);
                }
                (None, Some((var, source))) if var.required => env.missing.push((name.to_owned(), source.clone())),
                (None, _) => {}
            }
        }
        Ok(env)
    }

    /// The variables to pass to a program: every one except those the shell already sets.
    pub fn for_child(&self) -> Vec<(String, String)> {
        self.vars.iter().filter(|(_, var)| var.source != Source::Shell).map(|(k, var)| (k.clone(), var.value.clone())).collect()
    }

    /// Fails when a required variable is not set, naming each one and where it is declared.
    pub fn ensure_set(self) -> Result<Self> {
        if self.missing.is_empty() {
            return Ok(self);
        }
        let names: Vec<String> =
            self.missing.iter().map(|(name, source)| format!("`{name}` (declared in {})", source.describe())).collect();
        let (noun, pronoun) = if names.len() == 1 { ("variable", "it") } else { ("variables", "them") };
        bail!(
            "required environment {noun} not set: {}\nset {pronoun} in your shell or in a .env file, or give {pronoun} a `value` in kargo.toml",
            names.join(", ")
        );
    }

    /// Warnings for `.env` files that git tracks while they set secret-looking variables. Git is
    /// asked only about those files, and without git nothing counts as tracked.
    pub fn tracked_secret_warnings(&self, root: &Path) -> Vec<String> {
        self.files
            .iter()
            .filter(|f| !f.secret_keys.is_empty() && is_tracked(root, &f.file))
            .map(|f| {
                format!(
                    "`{}` is tracked by git but sets {}; keep secrets in a git-ignored `.local` file or in your shell",
                    f.file,
                    f.secret_keys.join(", ")
                )
            })
            .collect()
    }
}

/// Resolves the environment of `ws` for the profile named `profile` (`dev` or `release`), with
/// the variables that are required but unset left in [`ProjectEnv::missing`].
pub fn resolve(ws: &Workspace, profile: &str, opts: &EnvOptions) -> Result<ProjectEnv> {
    let manifest = ws.manifest();
    let profile_env = if profile == "release" { &manifest.release_profile.env } else { &manifest.dev_profile.env };
    // Variables that are not valid Unicode are left out: `.env` values are text.
    let shell: BTreeMap<String, String> =
        std::env::vars_os().filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?))).collect();
    ProjectEnv::load(ws.root(), profile, &manifest.env, profile_env, opts, &shell)
}

/// The keys of `.env` entries that look like secrets and have a value.
fn secret_keys(entries: &[(String, String, usize)]) -> Vec<String> {
    entries
        .iter()
        .filter(|(k, v, _)| is_secret_name(k) && !v.is_empty())
        .map(|(k, _, _)| k.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether a variable name looks like it holds a secret: it contains `SECRET`, `TOKEN`,
/// `PASSWORD`, `PASSWD` or `CREDENTIAL`, or it is `KEY` or ends in `_KEY`. Case does not matter.
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["SECRET", "TOKEN", "PASSWORD", "PASSWD", "CREDENTIAL"].iter().any(|word| upper.contains(word))
        || upper == "KEY"
        || upper.ends_with("_KEY")
}

/// Whether git tracks `file` (relative to `root`). Without git, or outside a repository, nothing is tracked.
fn is_tracked(root: &Path, file: &str) -> bool {
    Command::new("git")
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(file)
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The variable names in the package's `.env.example`, or `None` when it has none.
pub fn example_keys(root: &Path) -> Result<Option<Vec<String>>> {
    let text = match std::fs::read_to_string(root.join(".env.example")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("failed to read .env.example"),
    };
    let entries = parse_dotenv(".env.example", &text, &|_| None)?;
    Ok(Some(entries.into_iter().map(|(key, _, _)| key).collect()))
}

/// The Kotlin package of the generated `BuildConfig`: the group and name, each segment made into
/// a valid identifier (`my-app` becomes `my_app`).
pub fn build_config_package(group: Option<&str>, name: &str) -> String {
    let full = match group {
        Some(group) => format!("{group}.{name}"),
        None => name.to_owned(),
    };
    full.split('.')
        .map(|segment| {
            let mut clean: String = segment.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
            if clean.is_empty() || clean.starts_with(|c: char| c.is_ascii_digit()) {
                clean.insert(0, '_');
            }
            package_segment(&clean)
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The Kotlin source of the `BuildConfig` object that `[env] expose` asks for: a `const val` for
/// each exposed variable that is set, and a `null` `val` for one that is not, so the build does not
/// fail for a variable that only some machines set.
pub fn build_config_source(package: &str, expose: &[String], env: &ProjectEnv) -> String {
    let mut lines = vec![
        "// Generated by Kargo from `[env] expose` in kargo.toml. The next build overwrites it.".to_owned(),
        format!("package {package}"),
        String::new(),
        "object BuildConfig {".to_owned(),
    ];
    for name in expose {
        let ident = if RESERVED_WORDS.contains(&name.as_str()) { format!("`{name}`") } else { name.clone() };
        lines.push(match env.vars.get(name) {
            Some(var) => format!("    const val {ident}: String = {}", kotlin_string(&var.value)),
            None => format!("    val {ident}: String? = null"),
        });
    }
    lines.push("}".to_owned());
    lines.push(String::new());
    lines.join("\n")
}

/// A Kotlin string literal with `value` as its text.
fn kotlin_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The value of a kargo.toml variable: `relative` values are resolved against the package root.
fn manifest_value(var: &EnvVar, root: &Path) -> String {
    let value = var.value.as_deref().unwrap_or_default();
    if var.relative { root.join(value).to_string_lossy().into_owned() } else { value.to_owned() }
}

/// Parses the text of a `.env` file into `(key, value, line)` entries, in file order. `known`
/// answers references to variables from outside the file.
///
/// Lines are `KEY=value`, with an optional `export` and `#` comments. Values may be in single
/// quotes (taken literally) or double quotes (escapes, and lines spanning several). Unquoted
/// values end at a ` #` comment. `${NAME}` and `${NAME:-default}` are replaced in unquoted and
/// double-quoted values, using earlier keys of the file first and then `known`.
fn parse_dotenv(file: &str, text: &str, known: &dyn Fn(&str) -> Option<String>) -> Result<Vec<(String, String, usize)>> {
    let lines: Vec<&str> = text.lines().collect();
    let mut entries: Vec<(String, String, usize)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line_no = i + 1;
        let line = lines[i].trim();
        i += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map_or(line, str::trim_start);
        let Some((key, rest)) = line.split_once('=') else {
            bail!("{file}:{line_no}: expected `KEY=value`");
        };
        let key = key.trim();
        if !is_env_name(key) {
            bail!("{file}:{line_no}: invalid variable name `{key}` (use letters, digits and `_`)");
        }
        let lookup = |name: &str| entries.iter().rev().find(|(k, _, _)| k == name).map(|(_, v, _)| v.clone()).or_else(|| known(name));
        let rest = rest.trim_start();
        let value = if let Some(body) = rest.strip_prefix('"') {
            let mut raw = body.to_owned();
            let end = loop {
                if let Some(end) = closing_quote(&raw) {
                    break end;
                }
                let Some(next) = lines.get(i) else {
                    bail!("{file}:{line_no}: unterminated double quote for `{key}`");
                };
                raw.push('\n');
                raw.push_str(next);
                i += 1;
            };
            interpolate(&unescape(&raw[..end]), &lookup)
        } else if let Some(body) = rest.strip_prefix('\'') {
            let Some(end) = body.find('\'') else {
                bail!("{file}:{line_no}: unterminated single quote for `{key}`");
            };
            body[..end].to_owned()
        } else {
            let comment = rest.char_indices().find(|&(at, c)| c == '#' && (at == 0 || rest[..at].ends_with(char::is_whitespace)));
            let unquoted = comment.map_or(rest, |(at, _)| &rest[..at]);
            interpolate(unquoted.trim_end(), &lookup)
        };
        entries.push((key.to_owned(), value, line_no));
    }
    Ok(entries)
}

/// The index of the first double quote in `text` that no backslash escapes.
fn closing_quote(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        match (escaped, c) {
            (true, _) => escaped = false,
            (false, '\\') => escaped = true,
            (false, '"') => return Some(at),
            _ => {}
        }
    }
    None
}

/// Replaces `\n`, `\t`, `\r`, `\"` and `\\` in a double-quoted value; other escapes stay as written.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(escaped @ ('"' | '\\')) => out.push(escaped),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Replaces each `${NAME}` and `${NAME:-default}` in `value`. An unset name becomes the default,
/// or nothing; `:-` also takes the default for an empty value. An unclosed `${` stays as written.
fn interpolate(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let Some(len) = rest[start..].find('}') else { break };
        out.push_str(&rest[..start]);
        let body = &rest[start + 2..start + len];
        let (name, default) = match body.split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (body, None),
        };
        let found = lookup(name).filter(|v| default.is_none() || !v.is_empty());
        out.push_str(&found.unwrap_or_else(|| default.unwrap_or_default().to_owned()));
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_known(_: &str) -> Option<String> {
        None
    }

    fn parse(text: &str) -> Vec<(String, String, usize)> {
        parse_dotenv(".env", text, &no_known).unwrap()
    }

    fn shell(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn var(value: &str) -> EnvVar {
        EnvVar { value: Some(value.to_owned()), ..Default::default() }
    }

    /// A table with the given variables and nothing else.
    fn table(vars: &[(&str, EnvVar)]) -> EnvTable {
        EnvTable { vars: vars.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(), ..Default::default() }
    }

    fn load(root: &Path, base: &EnvTable, profile: &EnvTable, opts: &EnvOptions, shell: &BTreeMap<String, String>) -> Result<ProjectEnv> {
        ProjectEnv::load(root, "dev", base, profile, opts, shell)
    }

    #[test]
    fn parses_comments_export_and_quotes() {
        let entries = parse("# comment\n\nexport A=1\nB = two  # trailing\nC='a # b\\n'\nD=\"x\\ty\\\"z\"\nE=\n");
        let values: Vec<(&str, &str, usize)> = entries.iter().map(|(k, v, l)| (k.as_str(), v.as_str(), *l)).collect();
        assert_eq!(values, [("A", "1", 3), ("B", "two", 4), ("C", "a # b\\n", 5), ("D", "x\ty\"z", 6), ("E", "", 7)]);
    }

    #[test]
    fn double_quotes_span_lines() {
        let entries = parse("KEY=\"first\nsecond\"\nNEXT=1\n");
        assert_eq!(entries, [("KEY".to_owned(), "first\nsecond".to_owned(), 1), ("NEXT".to_owned(), "1".to_owned(), 3)]);
    }

    #[test]
    fn interpolates_earlier_keys_and_known_values() {
        let known = |name: &str| (name == "HOME_DIR").then(|| "/home/me".to_owned());
        let entries =
            parse_dotenv(".env", "BASE=${HOME_DIR}/app\nLOG=${LEVEL:-info}\nURL=\"${BASE}:${EMPTY:-8080}\"\nRAW='${BASE}'\n", &known)
                .unwrap();
        let values: Vec<&str> = entries.iter().map(|(_, v, _)| v.as_str()).collect();
        assert_eq!(values, ["/home/me/app", "info", "/home/me/app:8080", "${BASE}"]);
    }

    #[test]
    fn rejects_malformed_lines() {
        assert!(parse_dotenv(".env", "NOT A PAIR", &no_known).is_err());
        assert!(parse_dotenv(".env", "1BAD=x", &no_known).is_err());
        assert!(parse_dotenv(".env", "A=\"open", &no_known).is_err());
        assert!(parse_dotenv(".env", "A='open", &no_known).is_err());
    }

    #[test]
    fn shell_wins_unless_forced() {
        let base = table(&[("LOG", var("debug")), ("FORCED", EnvVar { force: true, ..var("kargo") })]);
        let profile = table(&[("APP", var("release-app"))]);
        let env_shell = shell(&[("LOG", "trace"), ("FORCED", "shell")]);
        let env = load(Path::new("/nonexistent"), &base, &profile, &EnvOptions::default(), &env_shell).unwrap();
        assert_eq!(env.vars["LOG"], Var { value: "trace".to_owned(), source: Source::Shell });
        assert_eq!(env.vars["FORCED"], Var { value: "kargo".to_owned(), source: Source::Manifest("[env]".to_owned()) });
        assert_eq!(env.vars["APP"].source.describe(), "[profile.dev.env]");
        assert_eq!(env.for_child().len(), 2);
    }

    #[test]
    fn files_override_manifest_and_profile_overrides_base() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "A=file\nB=file\n").unwrap();
        std::fs::write(dir.path().join(".env.dev"), "B=dev-file\n").unwrap();
        std::fs::write(dir.path().join(".env.dev.local"), "C=local\n").unwrap();
        let base = table(&[("A", var("manifest")), ("C", var("manifest")), ("D", var("manifest"))]);
        let profile = table(&[("D", var("profile"))]);
        let env = load(dir.path(), &base, &profile, &EnvOptions::default(), &BTreeMap::new()).unwrap();
        assert_eq!(env.vars["A"], Var { value: "file".to_owned(), source: Source::File { file: ".env".to_owned(), line: 1 } });
        assert_eq!(env.vars["B"].value, "dev-file");
        assert_eq!(env.vars["C"].value, "local");
        assert_eq!(env.vars["D"].value, "profile");
        // `.env.release` is not read for the dev profile.
        std::fs::write(dir.path().join(".env.release"), "B=release\n").unwrap();
        let env = load(dir.path(), &base, &profile, &EnvOptions::default(), &BTreeMap::new()).unwrap();
        assert_eq!(env.vars["B"].value, "dev-file");
    }

    #[test]
    fn env_file_option_wins_and_no_files_skips_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "A=standard\n").unwrap();
        std::fs::write(dir.path().join("extra.env"), "A=extra\n").unwrap();
        let opts = EnvOptions { files: vec![dir.path().join("extra.env")], no_files: false };
        let env = load(dir.path(), &EnvTable::default(), &EnvTable::default(), &opts, &BTreeMap::new()).unwrap();
        assert_eq!(env.vars["A"].value, "extra");

        let opts = EnvOptions { files: Vec::new(), no_files: true };
        let env = load(dir.path(), &EnvTable::default(), &EnvTable::default(), &opts, &BTreeMap::new()).unwrap();
        assert!(env.vars.is_empty());

        let missing = EnvOptions { files: vec![dir.path().join("nope.env")], no_files: false };
        let err = load(dir.path(), &EnvTable::default(), &EnvTable::default(), &missing, &BTreeMap::new()).unwrap_err();
        assert!(err.to_string().contains("--env-file names"), "{err}");
    }

    #[test]
    fn env_files_are_read_after_the_standard_ones_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "A=standard\nB=standard\n").unwrap();
        std::fs::write(dir.path().join("shared.env"), "A=shared\nC=shared\n").unwrap();
        std::fs::write(dir.path().join("release.env"), "C=release\n").unwrap();
        let base = EnvTable { files: vec!["shared.env".to_owned()], ..Default::default() };
        let profile = EnvTable { files: vec!["release.env".to_owned()], ..Default::default() };
        let env = load(dir.path(), &base, &profile, &EnvOptions::default(), &BTreeMap::new()).unwrap();
        assert_eq!(env.vars["A"].value, "shared");
        assert_eq!(env.vars["B"].value, "standard");
        assert_eq!(env.vars["C"].value, "release");
        let names: Vec<&str> = env.files.iter().map(|f| f.file.as_str()).collect();
        assert_eq!(names, [".env", "shared.env", "release.env"]);

        // `--no-env-file` skips them too, and a named file that is missing is an error.
        let none = EnvOptions { files: Vec::new(), no_files: true };
        assert!(load(dir.path(), &base, &profile, &none, &BTreeMap::new()).unwrap().vars.is_empty());
        let missing = EnvTable { files: vec!["gone.env".to_owned()], ..Default::default() };
        let err = load(dir.path(), &missing, &EnvTable::default(), &EnvOptions::default(), &BTreeMap::new()).unwrap_err();
        assert!(err.to_string().contains("[env] env-files names"), "{err}");
    }

    #[test]
    fn required_variables_must_be_set() {
        let base = table(&[("DATABASE_URL", EnvVar { required: true, ..Default::default() })]);
        let env = load(Path::new("/nonexistent"), &base, &EnvTable::default(), &EnvOptions::default(), &BTreeMap::new()).unwrap();
        let err = env.ensure_set().unwrap_err().to_string();
        assert!(err.contains("`DATABASE_URL` (declared in [env])"), "{err}");

        let set = shell(&[("DATABASE_URL", "postgres://")]);
        let env = load(Path::new("/nonexistent"), &base, &EnvTable::default(), &EnvOptions::default(), &set).unwrap();
        assert!(env.ensure_set().is_ok());
    }

    #[test]
    fn relative_values_resolve_against_the_root() {
        let base = table(&[("CONFIG_DIR", EnvVar { relative: true, ..var("config") })]);
        let env = load(Path::new("/app"), &base, &EnvTable::default(), &EnvOptions::default(), &BTreeMap::new()).unwrap();
        assert_eq!(env.vars["CONFIG_DIR"].value, Path::new("/app").join("config").to_string_lossy());
    }

    #[test]
    fn exposed_names_are_resolved_even_from_the_shell_alone() {
        let base = EnvTable { expose: vec!["BUILD_ID".to_owned()], ..Default::default() };
        let env =
            load(Path::new("/nonexistent"), &base, &EnvTable::default(), &EnvOptions::default(), &shell(&[("BUILD_ID", "42")])).unwrap();
        assert_eq!(env.vars["BUILD_ID"], Var { value: "42".to_owned(), source: Source::Shell });
        assert!(env.for_child().is_empty());
    }

    #[test]
    fn build_config_source_has_constants_and_nullable_values() {
        let base = table(&[("APP_NAME", var("say \"hi\" $5\\")), ("class", var("x"))]);
        let env = load(Path::new("/nonexistent"), &base, &EnvTable::default(), &EnvOptions::default(), &BTreeMap::new()).unwrap();
        let expose = ["APP_NAME".to_owned(), "class".to_owned(), "OPTIONAL".to_owned()];
        let source = build_config_source("com.example.demo", &expose, &env);
        assert_eq!(
            source,
            "// Generated by Kargo from `[env] expose` in kargo.toml. The next build overwrites it.\n\
             package com.example.demo\n\nobject BuildConfig {\n    \
             const val APP_NAME: String = \"say \\\"hi\\\" \\$5\\\\\"\n    \
             const val `class`: String = \"x\"\n    val OPTIONAL: String? = null\n}\n"
        );
    }

    #[test]
    fn build_config_package_makes_valid_identifiers() {
        assert_eq!(build_config_package(Some("com.example"), "demo"), "com.example.demo");
        assert_eq!(build_config_package(None, "my-app"), "my_app");
        assert_eq!(build_config_package(Some("org.x"), "2fa"), "org.x._2fa");
        assert_eq!(build_config_package(None, "class"), "class_");
    }

    #[test]
    fn recognises_secret_names() {
        for name in ["API_TOKEN", "db_password", "CLIENT_SECRET", "PRIVATE_KEY", "KEY", "SIGNING_KEY", "AWS_CREDENTIALS"] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in ["APP_NAME", "LOG_LEVEL", "MONKEY", "PORT"] {
            assert!(!is_secret_name(name), "{name}");
        }
    }

    #[test]
    fn example_keys_are_read_when_present() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(example_keys(dir.path()).unwrap(), None);
        std::fs::write(dir.path().join(".env.example"), "# keys\nDATABASE_URL=\nexport API_TOKEN=\n").unwrap();
        assert_eq!(example_keys(dir.path()).unwrap(), Some(vec!["DATABASE_URL".to_owned(), "API_TOKEN".to_owned()]));
    }

    #[test]
    fn secret_files_are_recorded_for_the_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "API_TOKEN=abc\nEMPTY_TOKEN=\nAPP=1\n").unwrap();
        let env = load(dir.path(), &EnvTable::default(), &EnvTable::default(), &EnvOptions::default(), &BTreeMap::new()).unwrap();
        assert_eq!(env.files, [LoadedFile { file: ".env".to_owned(), secret_keys: vec!["API_TOKEN".to_owned()] }]);
        // Outside a git repository nothing is tracked, so there is nothing to warn about.
        assert!(env.tracked_secret_warnings(dir.path()).is_empty());
    }
}
