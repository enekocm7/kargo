//! Single-file Kotlin scripts, run by `kargo script`: the `@file:` annotations at the top of a
//! `.kts` file, and the `/* kargo ... */` front matter of a `.kt` file. This module only reads
//! them; running a script is `kargo_ops::script`.

use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use kargo_util::fs::dunce_canonicalize;

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
    ("Target", "the target to build for: \"jvm\" or \"native\""),
    ("Import", "other scripts to share code with, relative to this one"),
];

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
    pub target: Option<ScriptTarget>,
    /// Extra Kotlin files compiled with a `.kt` file (`sources`), as written: files and
    /// directories relative to it.
    pub sources: Vec<String>,
}

impl Settings {
    /// Adds what an imported script needs: its dependencies, repositories and frameworks. How the
    /// whole script is compiled and run (Kotlin, JDK and target) comes from the script that runs.
    fn add_imported(&mut self, imported: Settings) {
        self.dependencies.extend(imported.dependencies);
        self.repositories.extend(imported.repositories);
        self.frameworks.extend(imported.frameworks);
    }
}

/// The target `kargo script` builds a file for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScriptTarget {
    /// JVM bytecode, run with `java` (the default).
    #[default]
    Jvm,
    /// A native executable for this machine, built with the Kotlin/Native toolchain.
    Native,
}

impl ScriptTarget {
    /// Parses `jvm` or `native`, as `@file:Target` and the `target` front matter key take them.
    pub fn parse(text: &str) -> Result<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "jvm" => Ok(Self::Jvm),
            "native" => Ok(Self::Native),
            other => bail!("invalid target `{other}`: expected \"jvm\" or \"native\""),
        }
    }
}

impl fmt::Display for ScriptTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Jvm => f.write_str("jvm"),
            Self::Native => f.write_str("native"),
        }
    }
}

impl std::str::FromStr for ScriptTarget {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        Self::parse(text)
    }
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
        match read_annotation(text, pos) {
            Ok((args, end)) if known => {
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

/// A script imported with `@file:Import`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// The file, canonical, so a script imported twice is read once.
    pub path: PathBuf,
    pub text: String,
    pub script: KtsScript,
}

/// A `.kts` script ready to compile: what it and the scripts it imports declare, and the code of all
/// of them in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedScript {
    pub settings: Settings,
    /// The script's code. With imports, it is the code of the imported scripts followed by the
    /// script's own, under one list of `@file:` annotations and import directives.
    pub code: String,
    /// The scripts it imports, directly and through each other, each after the ones it imports.
    pub imports: Vec<PathBuf>,
}

/// Reads the `.kts` script `text` of the file at `path` and the scripts it imports with
/// `@file:Import`, which `path` is the directory of. Their dependencies, repositories and
/// frameworks are added to the script's own; the Kotlin version, JDK and target are the script's.
/// `-e` code has no file, so it cannot import.
pub fn resolve_kts(path: Option<&Path>, text: &str) -> Result<ResolvedScript> {
    let parsed = parse_kts(text)?;
    let mut settings = settings_from_annotations(text, &parsed.annotations)?;
    let imported = match path {
        Some(path) => imported_scripts(path, text, &parsed)?,
        None if parsed.annotations.iter().any(|a| a.name == "Import") => {
            bail!("@file:Import needs a script file: `-e` code has no directory to import from")
        }
        None => Vec::new(),
    };
    for script in &imported {
        let declared = settings_from_annotations(&script.text, &script.script.annotations)
            .with_context(|| format!("invalid header in {}", script.path.display()))?;
        settings.add_imported(declared);
    }
    let code = merge(&parsed, &imported);
    Ok(ResolvedScript { settings, code, imports: imported.into_iter().map(|script| script.path).collect() })
}

/// The scripts that `text`, the `.kts` script at `path`, imports; empty when it does not parse or an
/// import cannot be read. Editors and `kargo tidy` use it to find the files a script is made of.
pub fn imported_files(path: &Path, text: &str) -> Vec<PathBuf> {
    let Ok(script) = parse_kts(text) else { return Vec::new() };
    imported_scripts(path, text, &script).map_or_else(|_| Vec::new(), |found| found.into_iter().map(|script| script.path).collect())
}

/// The imports of the script at `path` that cannot be resolved, each with the annotation to blame.
pub fn import_errors(path: &Path, text: &str) -> Vec<(Range<usize>, anyhow::Error)> {
    let Ok(script) = parse_kts(text) else { return Vec::new() };
    let mut errors = Vec::new();
    for annotation in script.annotations.iter().filter(|a| a.name == "Import") {
        for arg in &annotation.args {
            let mut stack = vec![dunce_canonicalize(path)];
            if let Err(error) = import_one(path, text, annotation, arg, &mut stack, &mut Vec::new()) {
                errors.push((annotation.span.clone(), error));
            }
        }
    }
    errors
}

fn imported_scripts(path: &Path, text: &str, script: &KtsScript) -> Result<Vec<Imported>> {
    let mut out = Vec::new();
    collect_imports(path, text, script, &mut vec![dunce_canonicalize(path)], &mut out)?;
    Ok(out)
}

/// Adds the scripts imported by `script`, the text of the file at `path`, to `out`. `stack` holds the
/// scripts whose imports are being read, to find a cycle.
fn collect_imports(path: &Path, text: &str, script: &KtsScript, stack: &mut Vec<PathBuf>, out: &mut Vec<Imported>) -> Result<()> {
    for annotation in script.annotations.iter().filter(|a| a.name == "Import") {
        for arg in &annotation.args {
            import_one(path, text, annotation, arg, stack, out)?;
        }
    }
    Ok(())
}

/// Reads the script that `arg` of `annotation` names, relative to the script at `path`, and the
/// scripts it imports, then adds them to `out`.
fn import_one(
    path: &Path,
    text: &str,
    annotation: &Annotation,
    arg: &str,
    stack: &mut Vec<PathBuf>,
    out: &mut Vec<Imported>,
) -> Result<()> {
    let line = line_of(text, annotation.span.start);
    let target = path.parent().unwrap_or(Path::new("")).join(arg);
    let id = dunce_canonicalize(&target);
    if let Some(start) = stack.iter().position(|p| *p == id) {
        let chain: Vec<String> =
            stack[start..].iter().chain([&id]).map(|p| p.file_name().unwrap_or(p.as_os_str()).to_string_lossy().into_owned()).collect();
        bail!("line {line}: import cycle: {}", chain.join(" -> "));
    }
    if out.iter().any(|imported| imported.path == id) {
        return Ok(());
    }
    if target.extension().is_none_or(|e| e != "kts") {
        bail!("line {line}: cannot import `{arg}`: only .kts scripts can be imported");
    }
    let imported_text = std::fs::read_to_string(&target)
        .with_context(|| format!("line {line}: cannot import `{arg}`: failed to read {}", target.display()))?;
    let invalid = || format!("invalid header in {}", target.display());
    let script = parse_kts(&imported_text).with_context(invalid)?;
    stack.push(id.clone());
    collect_imports(&target, &imported_text, &script, stack, out).with_context(invalid)?;
    stack.pop();
    out.push(Imported { path: id, text: imported_text, script });
    Ok(())
}

/// The code of `root` with the scripts it imports. The `@file:` annotations and import directives of
/// all of them open the file, since Kotlin allows them nowhere else. Then come the imported scripts,
/// each after the ones it imports, and the script itself, so top-level code runs in that order.
/// Without imports the code is the script's own.
fn merge(root: &KtsScript, imported: &[Imported]) -> String {
    if imported.is_empty() {
        return root.code.clone();
    }
    let mut annotations: Vec<&str> = Vec::new();
    let mut directives: Vec<&str> = Vec::new();
    let mut bodies = String::new();
    let parts = imported.iter().map(|script| (Some(&script.path), &script.script.code)).chain([(None, &root.code)]);
    for (path, code) in parts {
        let header = split_header(code);
        for (list, found) in [(&mut annotations, header.annotations), (&mut directives, header.imports)] {
            for item in found {
                if !list.contains(&item) {
                    list.push(item);
                }
            }
        }
        if let Some(path) = path {
            bodies.push_str(&format!("// imported from {}\n", path.display()));
        }
        bodies.push_str(&header.body);
        bodies.push('\n');
    }
    let mut out = String::new();
    for line in annotations.into_iter().chain(directives) {
        out.push_str(line);
        out.push('\n');
    }
    out + &bodies
}

/// The head of a script's code: its `@file:` annotations and import directives, and the rest.
struct Header<'a> {
    annotations: Vec<&'a str>,
    imports: Vec<&'a str>,
    /// The code without the head and the shebang line, which are blanked out.
    body: String,
}

fn split_header(code: &str) -> Header<'_> {
    let mut pos = shebang_end(code);
    let mut spans: Vec<Range<usize>> = std::iter::once(0..pos).collect();
    let (mut annotations, mut imports) = (Vec::new(), Vec::new());
    loop {
        pos = skip_trivia(code, pos);
        let rest = &code[pos..];
        let end = if rest.starts_with(FILE_PREFIX) {
            let Ok((_, end)) = read_annotation(code, pos) else { break };
            annotations.push(&code[pos..end]);
            end
        } else if rest.strip_prefix("import").is_some_and(|after| after.starts_with(char::is_whitespace)) {
            let end = pos + rest.find('\n').unwrap_or(rest.len());
            imports.push(code[pos..end].trim());
            end
        } else {
            break;
        };
        spans.push(pos..end);
        pos = end;
    }
    Header { annotations, imports, body: blank(code, &spans) }
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
        "Target" => settings.target = Some(ScriptTarget::parse(&single(annotation)?)?),
        // The imported scripts are read by `resolve_kts`.
        "Import" => {}
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
            ("target", toml::Value::String(v)) => settings.target = Some(ScriptTarget::parse(v)?),
            ("sources", toml::Value::Array(items)) => {
                for item in items {
                    settings.sources.push(item.as_str().context("sources must be strings")?.to_owned());
                }
            }
            (other, _) => {
                bail!(
                    "unknown key `{other}` in the kargo front matter; expected kotlin, jdk, repositories, frameworks, target, sources or dependencies"
                )
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
