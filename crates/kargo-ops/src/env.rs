//! The project environment: `[env]` and `[profile.<name>.env]` from kargo.toml and the `.env`
//! files of a package, layered under the shell's environment. `kargo run`, `kargo test`, tasks
//! and hooks start their programs with it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use kargo_manifest::{EnvVar, is_env_name};

use crate::workspace::Workspace;

/// The `.env` files of a package, in increasing priority: `.env`, `.env.<profile>`,
/// `.env.local` and `.env.<profile>.local`. A missing file is skipped.
pub fn standard_files(profile: &str) -> [String; 4] {
    [".env".to_owned(), format!(".env.{profile}"), ".env.local".to_owned(), format!(".env.{profile}.local")]
}

/// Which `.env` files a command reads (`--env-file`, `--no-env-file`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvOptions {
    /// Files named with `--env-file`. They are read after the standard files, so they win over them.
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

/// The variables a program starts with, resolved from every source.
#[derive(Debug, Default)]
pub struct ProjectEnv {
    /// Every variable some source declares, with the value that wins.
    pub vars: BTreeMap<String, Var>,
    /// Required variables that no source sets, with the place that declares them.
    pub missing: Vec<(String, Source)>,
}

impl ProjectEnv {
    /// Resolves the environment of the package at `root` for `profile` (`dev` or `release`).
    ///
    /// From the lowest priority to the highest: `[env]`, `[profile.<profile>.env]`, the `.env`
    /// files in order, then the shell. A variable with `force = true` in kargo.toml beats the
    /// shell and the files too. `shell` is the shell's environment.
    pub fn load(
        root: &Path,
        profile: &str,
        base: &BTreeMap<String, EnvVar>,
        profile_env: &BTreeMap<String, EnvVar>,
        opts: &EnvOptions,
        shell: &BTreeMap<String, String>,
    ) -> Result<Self> {
        let mut paths: Vec<(PathBuf, bool)> = Vec::new();
        if !opts.no_files {
            paths.extend(standard_files(profile).into_iter().map(|name| (root.join(name), false)));
        }
        paths.extend(opts.files.iter().map(|path| (path.clone(), true)));

        let mut files: BTreeMap<String, Var> = BTreeMap::new();
        for (path, named) in paths {
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && !named => continue,
                Err(e) => return Err(e).with_context(|| format!("failed to read {}", path.display())),
            };
            let file = path.strip_prefix(root).unwrap_or(&path).display().to_string();
            let known = |name: &str| files.get(name).map(|var| var.value.clone()).or_else(|| shell.get(name).cloned());
            for (key, value, line) in parse_dotenv(&file, &text, &known)? {
                files.insert(key, Var { value, source: Source::File { file: file.clone(), line } });
            }
        }

        // `[profile.<name>.env]` replaces `[env]` variable by variable.
        let mut manifest: BTreeMap<&str, (&EnvVar, Source)> = BTreeMap::new();
        for (name, var) in base {
            manifest.insert(name, (var, Source::Manifest("[env]".to_owned())));
        }
        let section = format!("[profile.{profile}.env]");
        for (name, var) in profile_env {
            manifest.insert(name, (var, Source::Manifest(section.clone())));
        }

        let names: BTreeSet<&str> = manifest.keys().copied().chain(files.keys().map(String::as_str)).collect();
        let mut env = ProjectEnv::default();
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
        let mut base = BTreeMap::new();
        base.insert("LOG".to_owned(), var("debug"));
        base.insert("FORCED".to_owned(), EnvVar { force: true, ..var("kargo") });
        let mut profile = BTreeMap::new();
        profile.insert("APP".to_owned(), var("release-app"));
        let env_shell = shell(&[("LOG", "trace"), ("FORCED", "shell")]);
        let env = ProjectEnv::load(Path::new("/nonexistent"), "dev", &base, &profile, &EnvOptions::default(), &env_shell).unwrap();
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
        let mut base = BTreeMap::new();
        base.insert("A".to_owned(), var("manifest"));
        base.insert("C".to_owned(), var("manifest"));
        base.insert("D".to_owned(), var("manifest"));
        let mut profile = BTreeMap::new();
        profile.insert("D".to_owned(), var("profile"));
        let env = ProjectEnv::load(dir.path(), "dev", &base, &profile, &EnvOptions::default(), &BTreeMap::new()).unwrap();
        assert_eq!(env.vars["A"], Var { value: "file".to_owned(), source: Source::File { file: ".env".to_owned(), line: 1 } });
        assert_eq!(env.vars["B"].value, "dev-file");
        assert_eq!(env.vars["C"].value, "local");
        assert_eq!(env.vars["D"].value, "profile");
        // `.env.release` is not read for the dev profile.
        std::fs::write(dir.path().join(".env.release"), "B=release\n").unwrap();
        let env = ProjectEnv::load(dir.path(), "dev", &base, &profile, &EnvOptions::default(), &BTreeMap::new()).unwrap();
        assert_eq!(env.vars["B"].value, "dev-file");
    }

    #[test]
    fn env_file_option_wins_and_no_files_skips_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "A=standard\n").unwrap();
        std::fs::write(dir.path().join("extra.env"), "A=extra\n").unwrap();
        let opts = EnvOptions { files: vec![dir.path().join("extra.env")], no_files: false };
        let env = ProjectEnv::load(dir.path(), "dev", &BTreeMap::new(), &BTreeMap::new(), &opts, &BTreeMap::new()).unwrap();
        assert_eq!(env.vars["A"].value, "extra");

        let opts = EnvOptions { files: Vec::new(), no_files: true };
        let env = ProjectEnv::load(dir.path(), "dev", &BTreeMap::new(), &BTreeMap::new(), &opts, &BTreeMap::new()).unwrap();
        assert!(env.vars.is_empty());

        let missing = EnvOptions { files: vec![dir.path().join("nope.env")], no_files: false };
        assert!(ProjectEnv::load(dir.path(), "dev", &BTreeMap::new(), &BTreeMap::new(), &missing, &BTreeMap::new()).is_err());
    }

    #[test]
    fn required_variables_must_be_set() {
        let mut base = BTreeMap::new();
        base.insert("DATABASE_URL".to_owned(), EnvVar { required: true, ..Default::default() });
        let env =
            ProjectEnv::load(Path::new("/nonexistent"), "dev", &base, &BTreeMap::new(), &EnvOptions::default(), &BTreeMap::new()).unwrap();
        let err = env.ensure_set().unwrap_err().to_string();
        assert!(err.contains("`DATABASE_URL` (declared in [env])"), "{err}");

        let set = shell(&[("DATABASE_URL", "postgres://")]);
        let env = ProjectEnv::load(Path::new("/nonexistent"), "dev", &base, &BTreeMap::new(), &EnvOptions::default(), &set).unwrap();
        assert!(env.ensure_set().is_ok());
    }

    #[test]
    fn relative_values_resolve_against_the_root() {
        let mut base = BTreeMap::new();
        base.insert("CONFIG_DIR".to_owned(), EnvVar { relative: true, ..var("config") });
        let env = ProjectEnv::load(Path::new("/app"), "dev", &base, &BTreeMap::new(), &EnvOptions::default(), &BTreeMap::new()).unwrap();
        assert_eq!(env.vars["CONFIG_DIR"].value, Path::new("/app").join("config").to_string_lossy());
    }
}
