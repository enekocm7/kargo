//! Single-file Kotlin scripts, run by `kargo script`: the `@file:` annotations at the top of a
//! `.kts` file, and the `/* kargo ... */` front matter of a `.kt` file. This module only reads
//! them; running a script is `kargo_ops::script`.

use std::fmt;
use std::ops::Range;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::JdkRequest;

/// The prefix every script annotation starts with.
const FILE_PREFIX: &str = "@file:";

/// The `@file:` annotations Kargo acts on. Each one is removed from a `.kts` script before it is
/// compiled, so the Kotlin compiler never needs the annotation classes of `kotlin-main-kts`.
pub const ANNOTATIONS: &[(&str, &str)] = &[
    ("DependsOn", "a Maven dependency as \"group:artifact:version\""),
    ("Repository", "an extra Maven repository URL, or \"mavenCentral\""),
    ("Kotlin", "the Kotlin version to compile the script with"),
    ("Jdk", "the JDK to compile and run the script with: a major version or \"system\""),
    ("Framework", "a built-in framework such as \"serialization\" or \"coroutines\""),
];

/// The `kotlin-main-kts` annotation Kargo does not support yet.
const UNSUPPORTED_ANNOTATIONS: &[&str] = &["Import"];

/// One `@file:` annotation Kargo understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    /// The name after `@file:`, such as `DependsOn`.
    pub name: String,
    /// The string arguments in order; an array literal contributes its elements.
    pub args: Vec<String>,
    /// Byte range of the whole annotation, from `@file:` to the closing parenthesis.
    pub span: Range<usize>,
}

/// A `.kts` script split into the annotations Kargo understands and the code that remains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KtsScript {
    pub annotations: Vec<Annotation>,
    /// The file with those annotations blanked out. Newlines are kept, so line numbers in
    /// compiler messages still match the file.
    pub code: String,
}

/// A `g:a:v` Maven coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coordinate {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

impl Coordinate {
    /// Parses `group:artifact:version`, rejecting a missing version.
    pub fn parse(text: &str) -> Result<Self> {
        let parts: Vec<&str> = text.split(':').collect();
        match parts.as_slice() {
            [group, artifact, version] if [group, artifact, version].iter().all(|p| !p.trim().is_empty()) => {
                Ok(Self { group: group.trim().to_owned(), artifact: artifact.trim().to_owned(), version: version.trim().to_owned() })
            }
            _ => bail!("invalid dependency `{text}`: expected \"group:artifact:version\", with the version pinned"),
        }
    }
}

impl fmt::Display for Coordinate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.artifact, self.version)
    }
}

/// What a script declares about the environment it runs in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub dependencies: Vec<Coordinate>,
    /// Extra repository URLs, or `mavenCentral` for the default one.
    pub repositories: Vec<String>,
    pub kotlin: Option<String>,
    pub jdk: Option<JdkRequest>,
    pub frameworks: Vec<String>,
}

/// Whether a `.kts` file is a Kargo script rather than a build script of some other tool. Outside
/// a project every `.kts` file is one, since nothing else would run it. Inside a project only a
/// file whose shebang runs `kargo script` is, because Kargo ignores the project's other scripts.
pub fn is_kargo_script(text: &str, in_project: bool) -> bool {
    !in_project || shebang_runs_kargo_script(text)
}

/// The scripts a manifest's tasks and hook plugins run (`script`), as written, relative to the
/// manifest's directory: the ones in `[tasks]` and `[plugins]` and in `[workspace.tasks]` and
/// `[workspace.plugins]`. They are Kargo scripts whatever their first line says, so an editor can
/// treat them as such. Read from the text alone, so it works for a manifest that does not validate.
pub fn manifest_scripts(manifest: &str) -> Vec<PathBuf> {
    let Ok(table) = manifest.parse::<toml::Table>() else { return Vec::new() };
    let sections = [
        table.get("tasks"),
        table.get("plugins"),
        table.get("workspace").and_then(|w| w.get("tasks")),
        table.get("workspace").and_then(|w| w.get("plugins")),
    ];
    sections
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
        .flat_map(|entries| entries.values())
        .filter_map(|entry| entry.get("script")?.as_str())
        .map(PathBuf::from)
        .collect()
}

fn shebang_runs_kargo_script(text: &str) -> bool {
    let Some(line) = text.strip_prefix("#!").map(|rest| rest.lines().next().unwrap_or("")) else { return false };
    let words: Vec<&str> = line.split_whitespace().collect();
    words.windows(2).any(|w| w[0].rsplit('/').next() == Some("kargo") && w[1] == "script")
}

/// Splits a `.kts` script into its annotations and code.
///
/// Only the header is read: the annotations before the first declaration or statement. Those
/// Kargo knows are returned and blanked out; any other `@file:` annotation stays in the code for
/// the Kotlin compiler. A known annotation with a malformed argument is an error.
pub fn parse_kts(text: &str) -> Result<KtsScript> {
    let mut annotations = Vec::new();
    let mut spans = Vec::new();
    let mut pos = shebang_end(text);
    loop {
        pos = skip_trivia(text, pos);
        if !text[pos..].starts_with(FILE_PREFIX) {
            break;
        }
        let name_start = pos + FILE_PREFIX.len();
        let name_len: usize = text[name_start..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum();
        let name = &text[name_start..name_start + name_len];
        let known = ANNOTATIONS.iter().any(|(n, _)| *n == name);
        let unsupported = UNSUPPORTED_ANNOTATIONS.contains(&name);
        match read_annotation(text, pos) {
            Ok((args, end)) if known || unsupported => {
                if unsupported {
                    bail!("line {}: @file:{name} is not supported yet; put the code in the script or in a project", line_of(text, pos));
                }
                let annotation = Annotation { name: name.to_owned(), args, span: pos..end };
                spans.push(annotation.span.clone());
                annotations.push(annotation);
                pos = end;
            }
            // Another annotation: the compiler checks it, so it stays where it is.
            Ok((_, end)) => pos = end,
            Err(error) if known => return Err(error.context(format!("line {}: invalid @file:{name}", line_of(text, pos)))),
            // Not an annotation Kargo can read: leave the rest of the file to the compiler.
            Err(_) => break,
        }
    }
    Ok(KtsScript { annotations, code: blank(text, &spans) })
}

/// Settings declared by the annotations of a `.kts` script in `text`. An error names the line of the
/// annotation that causes it.
pub fn settings_from_annotations(text: &str, annotations: &[Annotation]) -> Result<Settings> {
    let mut settings = Settings::default();
    for annotation in annotations {
        apply(&mut settings, annotation)
            .with_context(|| format!("line {}: invalid @file:{}", line_of(text, annotation.span.start), annotation.name))?;
    }
    Ok(settings)
}

fn apply(settings: &mut Settings, annotation: &Annotation) -> Result<()> {
    match annotation.name.as_str() {
        "DependsOn" => {
            for arg in &annotation.args {
                settings.dependencies.push(Coordinate::parse(arg)?);
            }
        }
        "Repository" => {
            for arg in &annotation.args {
                settings.repositories.push(repository(arg)?);
            }
        }
        "Kotlin" => settings.kotlin = Some(single(annotation)?),
        "Jdk" => settings.jdk = Some(single(annotation)?.parse()?),
        "Framework" => settings.frameworks.extend(annotation.args.iter().cloned()),
        other => bail!("unknown script annotation `@file:{other}`"),
    }
    Ok(())
}

/// Reads the front matter of a `.kt` file: a `/* kargo ... */` block comment at the top of the
/// file holding TOML. Returns `None` for a file without one.
///
/// ```kotlin
/// /* kargo
/// kotlin = "2.4.21"
/// [dependencies]
/// "com.github.ajalt.clikt:clikt" = "5.0.0"
/// */
/// ```
pub fn parse_kt_front_matter(text: &str) -> Result<Option<Settings>> {
    let pos = text.len() - text[shebang_end(text)..].trim_start().len();
    if !text[pos..].starts_with("/*") {
        return Ok(None);
    }
    let end = pos + block_comment_len(&text[pos..]);
    if !text[..end].ends_with("*/") || end - pos < 4 {
        bail!("the kargo front matter is not closed; end the /* kargo block with */");
    }
    let body = &text[pos + 2..end - 2];
    let Some(toml_text) = body.trim_start().strip_prefix("kargo").filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    else {
        return Ok(None);
    };
    let table: toml::Table = toml::from_str(toml_text).context("invalid kargo front matter")?;
    let mut settings = Settings::default();
    for (key, value) in &table {
        match (key.as_str(), value) {
            ("kotlin", toml::Value::String(v)) => settings.kotlin = Some(v.clone()),
            ("jdk", toml::Value::String(v)) => settings.jdk = Some(v.parse()?),
            ("repositories", toml::Value::Array(items)) => {
                for item in items {
                    settings.repositories.push(repository(item.as_str().context("repositories must be strings")?)?);
                }
            }
            ("frameworks", toml::Value::Array(items)) => {
                for item in items {
                    settings.frameworks.push(item.as_str().context("frameworks must be strings")?.to_owned());
                }
            }
            ("dependencies", toml::Value::Table(deps)) => {
                for (module, version) in deps {
                    let version = version.as_str().with_context(|| format!("version of `{module}` must be a string"))?;
                    settings.dependencies.push(Coordinate::parse(&format!("{module}:{version}"))?);
                }
            }
            (other, _) => {
                bail!("unknown key `{other}` in the kargo front matter; expected kotlin, jdk, repositories, frameworks or dependencies")
            }
        }
    }
    Ok(Some(settings))
}

fn single(annotation: &Annotation) -> Result<String> {
    match annotation.args.as_slice() {
        [value] => Ok(value.trim().to_owned()),
        _ => bail!("@file:{} takes exactly one string argument", annotation.name),
    }
}

/// `mavenCentral` or an http(s) URL, without a trailing slash.
fn repository(arg: &str) -> Result<String> {
    let arg = arg.trim();
    if arg == "mavenCentral" || arg.starts_with("https://") || arg.starts_with("http://") {
        return Ok(arg.trim_end_matches('/').to_owned());
    }
    bail!("invalid repository `{arg}`: expected an http(s) URL or \"mavenCentral\"")
}

/// Byte offset just past a `#!` line, or 0 when the file has none. The line stays in the code.
fn shebang_end(text: &str) -> usize {
    if !text.starts_with("#!") {
        return 0;
    }
    text.find('\n').unwrap_or(text.len())
}

/// Skips whitespace and comments, returning the offset of the next token.
fn skip_trivia(text: &str, mut pos: usize) -> usize {
    loop {
        let rest = &text[pos..];
        let trimmed = rest.trim_start();
        pos += rest.len() - trimmed.len();
        if trimmed.starts_with("//") {
            pos += trimmed.find('\n').unwrap_or(trimmed.len());
        } else if trimmed.starts_with("/*") {
            pos += block_comment_len(trimmed);
        } else {
            return pos;
        }
    }
}

/// Length of the block comment at the start of `text`, nesting included. An unterminated comment
/// runs to the end of the file.
fn block_comment_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    let (mut depth, mut i) = (0usize, 0usize);
    while i < bytes.len() {
        if bytes[i..].starts_with(b"/*") {
            depth += 1;
            i += 2;
        } else if bytes[i..].starts_with(b"*/") {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    text.len()
}

/// Reads the annotation at `start`: its arguments and the offset just past it.
fn read_annotation(text: &str, start: usize) -> Result<(Vec<String>, usize)> {
    let name_start = start + FILE_PREFIX.len();
    let name_len: usize = text[name_start..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum();
    if name_len == 0 {
        bail!("expected an annotation name after `@file:`");
    }
    let mut pos = skip_trivia(text, name_start + name_len);
    if !text[pos..].starts_with('(') {
        return Ok((Vec::new(), name_start + name_len));
    }
    pos += 1;
    let mut args = Vec::new();
    loop {
        pos = skip_trivia(text, pos);
        match text[pos..].chars().next() {
            Some(')') => return Ok((args, pos + 1)),
            Some('"') => {
                let (value, end) = string_at(text, pos)?;
                args.push(value);
                pos = end;
            }
            Some('[') => {
                pos += 1;
                loop {
                    pos = skip_trivia(text, pos);
                    match text[pos..].chars().next() {
                        Some(']') => {
                            pos += 1;
                            break;
                        }
                        Some('"') => {
                            let (value, end) = string_at(text, pos)?;
                            args.push(value);
                            pos = skip_trivia(text, end);
                        }
                        _ => bail!("expected a string in the array"),
                    }
                    if text[pos..].starts_with(',') {
                        pos += 1;
                    }
                }
            }
            _ => bail!("annotation arguments must be string literals"),
        }
        pos = skip_trivia(text, pos);
        if text[pos..].starts_with(',') {
            pos += 1;
        }
    }
}

/// The string literal at `pos` (regular or raw), its value and the offset just past it.
fn string_at(text: &str, pos: usize) -> Result<(String, usize)> {
    if text[pos..].starts_with("\"\"\"") {
        let body = pos + 3;
        let len = text[body..].find("\"\"\"").context("unterminated raw string")?;
        return Ok((text[body..body + len].to_owned(), body + len + 3));
    }
    let mut value = String::new();
    let mut chars = text[pos + 1..].char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((value, pos + 1 + i + 1)),
            '\\' => match chars.next() {
                Some((_, 'n')) => value.push('\n'),
                Some((_, 't')) => value.push('\t'),
                Some((_, other)) => value.push(other),
                None => break,
            },
            '\n' => break,
            other => value.push(other),
        }
    }
    bail!("unterminated string literal")
}

/// Replaces each span with spaces, keeping its newlines.
fn blank(text: &str, spans: &[Range<usize>]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for span in spans {
        out.push_str(&text[last..span.start]);
        out.extend(text[span.clone()].chars().map(|c| if c == '\n' { '\n' } else { ' ' }));
        last = span.end;
    }
    out.push_str(&text[last..]);
    out
}

/// The 1-based line of a byte offset.
pub fn line_of(text: &str, pos: usize) -> usize {
    text[..pos.min(text.len())].matches('\n').count() + 1
}
