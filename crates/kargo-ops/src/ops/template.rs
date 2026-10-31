//! `kargo new --template` and `kargo template`: projects from a template directory.
//!
//! A template is a directory with a `kargo-template.toml` that declares variables (prompted for, or
//! given with `--define`), which files to copy, which of them depend on an answer, and the steps to run
//! afterwards. Its files are copied with `{{variable}}` placeholders replaced in their names and
//! contents. The template comes from a local directory, a git repository (cloned under the Kargo home) or a
//! short name that `templates.toml` in the Kargo home maps to one of those.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Deserialize;

use super::new::{RESERVED_WORDS, package_segment, validate_group};
use kargo_manifest::{DEFAULT_KOTLIN_VERSION, MANIFEST_NAME, validate_package_name};
use kargo_util::config::Config;
use kargo_util::shell::Shell;

/// The file at the root of a template that describes it.
pub const TEMPLATE_MANIFEST: &str = "kargo-template.toml";

/// The file in the Kargo home that maps short names to template sources.
pub const REGISTRY_FILE: &str = "templates.toml";

/// Variables every template has. `name`, `group`, `kotlin` and `package` may also be declared, to be
/// prompted for or to get another default; `package_path` is always derived from `package`.
const BUILTIN_VARIABLES: &[&str] = &["name", "group", "package", "package_path", "kotlin"];

/// The filters of `{{variable | filter}}`.
const FILTERS: &[&str] = &["lower", "upper", "pascal", "camel", "snake", "kebab", "path", "package"];

// ---------------------------------------------------------------------------------------------------
// The template manifest
// ---------------------------------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawTemplate {
    #[serde(default)]
    template: RawInfo,
    #[serde(default)]
    variables: toml::Table,
    #[serde(default)]
    files: RawFiles,
    #[serde(default)]
    conditional: Vec<RawConditional>,
    #[serde(default)]
    steps: Vec<RawStep>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawInfo {
    name: Option<String>,
    description: Option<String>,
    /// Printed after the project is created.
    message: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawFiles {
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    raw: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawConditional {
    path: String,
    when: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawVariable {
    prompt: Option<String>,
    #[serde(rename = "type")]
    kind: Option<RawKind>,
    default: Option<toml::Value>,
    #[serde(default)]
    choices: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawKind {
    String,
    Bool,
    Choice,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawStep {
    builtin: Option<String>,
    run: Option<Vec<String>>,
    when: Option<String>,
}

/// The value a variable has in a placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Text(String),
    Bool(bool),
}

impl Value {
    fn text(&self) -> String {
        match self {
            Value::Text(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Value::Text(s) => !s.is_empty() && s != "false",
            Value::Bool(b) => *b,
        }
    }
}

/// The answers a template's placeholders and conditions read.
#[derive(Debug, Clone, Default)]
pub struct Answers(BTreeMap<String, Value>);

impl Answers {
    fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    fn text(&self, key: &str) -> String {
        self.get(key).map(Value::text).unwrap_or_default()
    }

    /// The answer to `key` as text, for tests and messages.
    pub fn value(&self, key: &str) -> Option<String> {
        self.get(key).map(Value::text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Text,
    Bool,
    Choice(Vec<String>),
}

#[derive(Debug, Clone)]
struct Variable {
    key: String,
    prompt: String,
    kind: Kind,
    /// The default as written in the template; it may contain placeholders.
    default: Option<String>,
}

/// A condition on the answers: `docker`, `!docker`, `ci == "github"` or `ci != github`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Condition {
    Is(String),
    Not(String),
    Equals { key: String, value: String, negate: bool },
}

impl Condition {
    fn parse(expr: &str, known: &dyn Fn(&str) -> bool) -> Result<Self> {
        let expr = expr.trim();
        let condition = if let Some((key, value)) = expr.split_once("!=") {
            Condition::Equals { key: key.trim().to_owned(), value: unquote(value), negate: true }
        } else if let Some((key, value)) = expr.split_once("==") {
            Condition::Equals { key: key.trim().to_owned(), value: unquote(value), negate: false }
        } else if let Some(key) = expr.strip_prefix('!') {
            Condition::Not(key.trim().to_owned())
        } else {
            Condition::Is(expr.to_owned())
        };
        let key = match &condition {
            Condition::Is(key) | Condition::Not(key) | Condition::Equals { key, .. } => key,
        };
        if !is_variable_name(key) {
            bail!("invalid condition `{expr}` (expected `name`, `!name`, `name == value` or `name != value`)");
        }
        if !known(key) {
            bail!("the condition `{expr}` uses the unknown variable `{key}`");
        }
        Ok(condition)
    }

    fn holds(&self, ctx: &Answers) -> bool {
        match self {
            Condition::Is(key) => ctx.get(key).is_some_and(Value::truthy),
            Condition::Not(key) => !ctx.get(key).is_some_and(Value::truthy),
            Condition::Equals { key, value, negate } => (ctx.text(key) == *value) != *negate,
        }
    }
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = value.strip_prefix(quote).and_then(|v| v.strip_suffix(quote)) {
            return inner.to_owned();
        }
    }
    value.to_owned()
}

fn is_variable_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase()) && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StepAction {
    GitInit,
    Fetch,
    Run(Vec<String>),
}

#[derive(Debug, Clone)]
struct Step {
    action: StepAction,
    when: Option<Condition>,
}

/// A step that will run, with the command line of a `run` step filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedStep {
    /// `git init`, unless `--no-vcs` or the project is a member of a workspace.
    GitInit,
    /// `kargo fetch` in the new project.
    Fetch,
    /// A command of the template: it needs `--allow-scripts`.
    Run(Vec<String>),
}

/// A template, loaded and checked.
#[derive(Debug)]
pub struct Template {
    root: PathBuf,
    pub name: Option<String>,
    pub description: Option<String>,
    message: Option<String>,
    variables: Vec<Variable>,
    include: Option<Gitignore>,
    exclude: Gitignore,
    raw: Gitignore,
    conditional: Vec<(Gitignore, Condition)>,
    steps: Vec<Step>,
}

fn matcher(patterns: &[String], what: &str) -> Result<Gitignore> {
    // Paths are matched relative to the template, so the root is the current directory, which `ignore` does not strip.
    let mut builder = GitignoreBuilder::new(".");
    for pattern in patterns {
        builder.add_line(None, pattern).with_context(|| format!("invalid pattern `{pattern}` in {what}"))?;
    }
    builder.build().with_context(|| format!("invalid patterns in {what}"))
}

fn matches(matcher: &Gitignore, rel: &str) -> bool {
    matcher.matched_path_or_any_parents(Path::new(rel), false).is_ignore()
}

impl Template {
    /// Loads the template in `root`, checking everything its `kargo-template.toml` declares.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(TEMPLATE_MANIFEST);
        if !path.is_file() {
            bail!("`{}` is not a template: it has no {TEMPLATE_MANIFEST}", root.display());
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
        let raw: RawTemplate = toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
        Self::from_raw(root, raw).with_context(|| format!("invalid template at {}", path.display()))
    }

    fn from_raw(root: &Path, raw: RawTemplate) -> Result<Self> {
        let mut variables = Vec::new();
        for (key, value) in raw.variables {
            if !is_variable_name(&key) {
                bail!("invalid variable name `{key}` (use lowercase letters, digits and `_`, starting with a letter)");
            }
            if matches!(key.as_str(), "name" | "package_path") {
                bail!(
                    "`{key}` cannot be declared: {}",
                    if key == "name" { "it is the folder's name, or `--name`" } else { "it is always derived from `package`" }
                );
            }
            let variable: RawVariable = value.try_into().with_context(|| format!("invalid variable `{key}`"))?;
            variables.push(Self::variable(key, variable)?);
        }
        let known = |key: &str| BUILTIN_VARIABLES.contains(&key) || variables.iter().any(|v| v.key == key);
        let mut conditional = Vec::new();
        for c in raw.conditional {
            let condition = Condition::parse(&c.when, &known)?;
            conditional.push((matcher(std::slice::from_ref(&c.path), "[[conditional]]")?, condition));
        }
        let mut steps = Vec::new();
        for step in raw.steps {
            let action = match (step.builtin.as_deref(), step.run) {
                (Some("git-init"), None) => StepAction::GitInit,
                (Some("fetch"), None) => StepAction::Fetch,
                (Some(other), None) => bail!("unknown builtin step `{other}` (available: git-init, fetch)"),
                (None, Some(argv)) if !argv.is_empty() => StepAction::Run(argv),
                (None, Some(_)) => bail!("a `run` step needs a command, e.g. run = [\"git\", \"init\"]"),
                _ => bail!("a step has either `builtin` or `run`"),
            };
            let when = step.when.as_deref().map(|w| Condition::parse(w, &known)).transpose()?;
            steps.push(Step { action, when });
        }
        let include = if raw.files.include.is_empty() { None } else { Some(matcher(&raw.files.include, "[files] include")?) };
        Ok(Self {
            root: root.to_path_buf(),
            name: raw.template.name,
            description: raw.template.description,
            message: raw.template.message,
            variables,
            include,
            exclude: matcher(&raw.files.exclude, "[files] exclude")?,
            raw: matcher(&raw.files.raw, "[files] raw")?,
            conditional,
            steps,
        })
    }

    fn variable(key: String, raw: RawVariable) -> Result<Variable> {
        let kind = match (raw.kind, &raw.default, raw.choices.is_empty()) {
            (Some(RawKind::Choice), _, false) | (None, _, false) => Kind::Choice(raw.choices.clone()),
            (Some(RawKind::Choice), _, true) => bail!("variable `{key}` is a choice but lists no `choices`"),
            (Some(_), _, false) => bail!("variable `{key}` has `choices` but is not of type \"choice\""),
            (Some(RawKind::Bool), _, true) | (None, Some(toml::Value::Boolean(_)), true) => Kind::Bool,
            (Some(RawKind::String), _, true) | (None, _, true) => Kind::Text,
        };
        let default = match (&raw.default, &kind) {
            (None, _) => None,
            (Some(toml::Value::Boolean(b)), Kind::Bool) => Some(b.to_string()),
            (Some(toml::Value::String(s)), Kind::Text) => Some(s.clone()),
            (Some(toml::Value::String(s)), Kind::Choice(choices)) => {
                if !choices.contains(s) {
                    bail!("the default `{s}` of variable `{key}` is not one of its choices ({})", choices.join(", "));
                }
                Some(s.clone())
            }
            (Some(toml::Value::String(s)), Kind::Bool) if parse_bool(s).is_some() => Some(parse_bool(s).unwrap_or_default().to_string()),
            (Some(other), _) => bail!("the default of variable `{key}` must be a {}, not `{other}`", kind_name(&kind)),
        };
        Ok(Variable { prompt: raw.prompt.unwrap_or_else(|| key.clone()), key, kind, default })
    }

    /// The variables declared, in the order they are asked.
    pub fn variable_names(&self) -> Vec<&str> {
        self.variables.iter().map(|v| v.key.as_str()).collect()
    }

    fn declared(&self, key: &str) -> Option<&Variable> {
        self.variables.iter().find(|v| v.key == key)
    }

    fn knows(&self, key: &str) -> bool {
        BUILTIN_VARIABLES.contains(&key) || self.declared(key).is_some()
    }

    // -----------------------------------------------------------------------------------------------
    // Answers
    // -----------------------------------------------------------------------------------------------

    /// Settles the value of every variable: `--define`, then what the user types when there is a terminal
    /// to ask on (unless `inputs.defaults`), then the default.
    pub fn resolve(&self, shell: &Shell, inputs: &Inputs) -> Result<Answers> {
        let ask = !inputs.defaults && shell.is_interactive();
        for key in inputs.defines.keys() {
            if key == "name" {
                bail!("the name of the package is the folder's; pass `--name <name>` to choose another");
            }
            if !self.knows(key) || key == "package_path" {
                let mut known: Vec<&str> = BUILTIN_VARIABLES.iter().copied().filter(|k| !matches!(*k, "package_path" | "name")).collect();
                let declared: Vec<&str> = self.variables.iter().map(|v| v.key.as_str()).filter(|k| !known.contains(k)).collect();
                known.extend(declared);
                let list = known.join(", ");
                return Err(kargo_util::help::unknown(format!("the template has no variable `{key}` (variables: {list})"), key, &known));
            }
        }
        if let Err(e) = validate_package_name(&inputs.name) {
            bail!("{e}; pass `--name <valid-name>` to name the package differently from the folder");
        }
        let mut ctx = Answers::default();
        ctx.0.insert("name".to_owned(), Value::Text(inputs.name.clone()));

        // A default given on the command line beats the template's, which beats Kargo's.
        let group = self.settle("group", inputs.group.clone(), Some("com.example".to_owned()), inputs, &ctx, shell, ask)?;
        validate_group(&group.text())?;
        ctx.0.insert("group".to_owned(), group);
        let kotlin = self.settle("kotlin", inputs.kotlin.clone(), Some(DEFAULT_KOTLIN_VERSION.to_owned()), inputs, &ctx, shell, ask)?;
        ctx.0.insert("kotlin".to_owned(), kotlin);

        let derived = ctx.text("group").split('.').chain([inputs.name.as_str()]).map(package_segment).collect::<Vec<_>>().join(".");
        let package = self.settle("package", None, Some(derived), inputs, &ctx, shell, ask)?;
        validate_package(&package.text())?;
        ctx.0.insert("package_path".to_owned(), Value::Text(package.text().replace('.', "/")));
        ctx.0.insert("package".to_owned(), package);

        for variable in &self.variables {
            if BUILTIN_VARIABLES.contains(&variable.key.as_str()) {
                continue;
            }
            let value = self.settle(&variable.key, None, None, inputs, &ctx, shell, ask)?;
            ctx.0.insert(variable.key.clone(), value);
        }
        Ok(ctx)
    }

    /// The value of `key`: a `--define`, else the answer to its prompt, else its default. `explicit` is a
    /// default given by a flag and `fallback` the one Kargo uses when the template says nothing.
    #[allow(clippy::too_many_arguments)]
    fn settle(
        &self,
        key: &str,
        explicit: Option<String>,
        fallback: Option<String>,
        inputs: &Inputs,
        ctx: &Answers,
        shell: &Shell,
        ask: bool,
    ) -> Result<Value> {
        let declared = self.declared(key);
        let defined = inputs.defines.get(key);
        let Some(variable) = declared else {
            let text = defined.cloned().or(explicit).or(fallback);
            return text.map(Value::Text).with_context(|| format!("the variable `{key}` needs a value; pass --define {key}=<value>"));
        };
        if let Some(text) = defined {
            return parse_answer(variable, text);
        }
        let default = match (explicit, &variable.default) {
            (Some(explicit), _) => Some(explicit),
            (None, Some(default)) => Some(render(default, ctx, &format!("the default of variable `{key}`"))?),
            (None, None) => fallback,
        };
        if ask {
            return self.ask(variable, default.as_deref(), shell);
        }
        match default {
            Some(default) => parse_answer(variable, &default),
            None => bail!("the variable `{key}` needs a value; pass --define {key}=<value>"),
        }
    }

    fn ask(&self, variable: &Variable, default: Option<&str>, shell: &Shell) -> Result<Value> {
        let cancelled = || anyhow::anyhow!("cancelled; no files were written");
        match &variable.kind {
            Kind::Choice(choices) => {
                // The default goes first, which is where the list starts.
                let mut items: Vec<String> = choices.clone();
                if let Some(i) = default.and_then(|d| items.iter().position(|c| c == d)) {
                    let first = items.remove(i);
                    items.insert(0, first);
                }
                let picked = shell.select(&variable.prompt, &items).context("could not ask for a choice")?.ok_or_else(cancelled)?;
                Ok(Value::Text(items[picked].clone()))
            }
            Kind::Bool => loop {
                let hint = match default.and_then(parse_bool) {
                    Some(true) => Some("y"),
                    Some(false) => Some("n"),
                    None => None,
                };
                let answer = shell.prompt(format!("{} (y/n)", variable.prompt), hint).ok_or_else(cancelled)?;
                match parse_bool(&answer) {
                    Some(b) => return Ok(Value::Bool(b)),
                    None => shell.error(format!("`{answer}` is not yes or no")),
                }
            },
            Kind::Text => loop {
                let answer = shell.prompt(&variable.prompt, default).ok_or_else(cancelled)?;
                if !answer.is_empty() {
                    return Ok(Value::Text(answer));
                }
                shell.error(format!("`{}` needs a value", variable.key));
            },
        }
    }

    // -----------------------------------------------------------------------------------------------
    // Files
    // -----------------------------------------------------------------------------------------------

    /// Renders the template's files for `ctx`, without writing anything.
    pub fn plan(&self, ctx: &Answers) -> Result<Vec<Output>> {
        let mut outputs: Vec<Output> = Vec::new();
        let walk = walkdir::WalkDir::new(&self.root).sort_by_file_name().follow_links(false);
        for entry in walk.into_iter().filter_entry(|e| e.depth() == 0 || e.file_name() != ".git") {
            let entry = entry.with_context(|| format!("failed to read the template at {}", self.root.display()))?;
            let rel = entry.path().strip_prefix(&self.root).unwrap_or(entry.path());
            if entry.path_is_symlink() {
                bail!("`{}` is a symbolic link; templates are plain files", rel.display());
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
            if rel == TEMPLATE_MANIFEST || !self.selects(&rel, ctx) {
                continue;
            }
            let Some(path) = rendered_path(&rel, ctx)? else { continue };
            let bytes = std::fs::read(entry.path()).with_context(|| format!("failed to read {}", entry.path().display()))?;
            let contents = match std::str::from_utf8(&bytes) {
                Ok(text) if !text.contains('\0') && !matches(&self.raw, &rel) => render(text, ctx, &rel)?.into_bytes(),
                _ => bytes,
            };
            if let Some(other) = outputs.iter().find(|o| o.path == path) {
                bail!("two files of the template are written to `{}` (one is `{}`; the other `{rel}`)", other.path.display(), other.source);
            }
            outputs.push(Output { path, contents, executable: is_executable(entry.path()), source: rel });
        }
        Ok(outputs)
    }

    /// Whether the template copies the file at `rel` for these answers.
    fn selects(&self, rel: &str, ctx: &Answers) -> bool {
        if self.include.as_ref().is_some_and(|include| !matches(include, rel)) || matches(&self.exclude, rel) {
            return false;
        }
        self.conditional.iter().all(|(pattern, condition)| !matches(pattern, rel) || condition.holds(ctx))
    }

    /// The steps to run for these answers, with the placeholders of their command lines filled in.
    pub fn steps(&self, ctx: &Answers) -> Result<Vec<PlannedStep>> {
        let mut planned = Vec::new();
        for step in &self.steps {
            if step.when.as_ref().is_some_and(|c| !c.holds(ctx)) {
                continue;
            }
            planned.push(match &step.action {
                StepAction::GitInit => PlannedStep::GitInit,
                StepAction::Fetch => PlannedStep::Fetch,
                StepAction::Run(argv) => PlannedStep::Run(argv.iter().map(|a| render(a, ctx, "a `run` step")).collect::<Result<Vec<_>>>()?),
            });
        }
        Ok(planned)
    }

    /// The message to print after the project is created, with the answers filled in.
    pub fn message(&self, ctx: &Answers) -> Result<Option<String>> {
        self.message.as_deref().map(|m| render(m, ctx, "the message of the template")).transpose()
    }
}

fn kind_name(kind: &Kind) -> &'static str {
    match kind {
        Kind::Text => "string",
        Kind::Bool => "boolean",
        Kind::Choice(_) => "string that is one of its choices",
    }
}

fn parse_bool(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" | "1" | "on" => Some(true),
        "false" | "no" | "n" | "0" | "off" => Some(false),
        _ => None,
    }
}

fn parse_answer(variable: &Variable, text: &str) -> Result<Value> {
    match &variable.kind {
        Kind::Text => Ok(Value::Text(text.to_owned())),
        Kind::Bool => {
            parse_bool(text).map(Value::Bool).with_context(|| format!("`{}` is a yes or no question; `{text}` is neither", variable.key))
        }
        Kind::Choice(choices) if choices.iter().any(|c| c == text) => Ok(Value::Text(text.to_owned())),
        Kind::Choice(choices) => Err(kargo_util::help::unknown(
            format!("`{text}` is not a choice of `{}` (choices: {})", variable.key, choices.join(", ")),
            text,
            choices,
        )),
    }
}

/// Checks a Kotlin package: dotted identifiers that are not keywords.
fn validate_package(package: &str) -> Result<()> {
    let ok = |s: &str| {
        s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !RESERVED_WORDS.contains(&s)
    };
    if package.is_empty() || !package.split('.').all(ok) {
        bail!("invalid package `{package}` (expected dotted identifiers such as `com.example.app`, none a Kotlin or Java keyword)");
    }
    Ok(())
}

/// What the user gave on the command line.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub name: String,
    pub group: Option<String>,
    pub kotlin: Option<String>,
    /// `--define key=value`.
    pub defines: BTreeMap<String, String>,
    /// `--defaults`: take every default and ask nothing.
    pub defaults: bool,
}

/// Reads the `key=value` pairs of `--define`.
pub fn parse_defines(defines: &[String]) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for define in defines {
        let Some((key, value)) = define.split_once('=') else {
            bail!("invalid --define `{define}` (expected `key=value`)");
        };
        let key = key.trim();
        if !is_variable_name(key) {
            bail!("invalid variable name `{key}` in --define (use lowercase letters, digits and `_`)");
        }
        out.insert(key.to_owned(), value.to_owned());
    }
    Ok(out)
}

/// A file the template writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Relative to the project root.
    pub path: PathBuf,
    pub contents: Vec<u8>,
    pub executable: bool,
    /// The template file it comes from, for messages.
    pub source: String,
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

/// The path of a template file in the new project: its placeholders filled in. `None` when a part of
/// it comes out empty, which leaves the file out. A rendered path may not leave the project.
fn rendered_path(rel: &str, ctx: &Answers) -> Result<Option<PathBuf>> {
    let mut path = PathBuf::new();
    for part in rel.split('/') {
        let rendered = render(part, ctx, rel)?;
        if rendered.is_empty() {
            return Ok(None);
        }
        for segment in rendered.split('/') {
            let bad = segment.is_empty()
                || matches!(segment, "." | "..")
                || segment.eq_ignore_ascii_case(".git")
                || segment.contains(['\\', ':', '\0']);
            if bad {
                bail!("the template file `{rel}` would be written to `{rendered}`, which is not a path inside the project");
            }
            path.push(segment);
        }
    }
    debug_assert!(path.components().all(|c| matches!(c, Component::Normal(_))));
    Ok(Some(path))
}

// ---------------------------------------------------------------------------------------------------
// Placeholders
// ---------------------------------------------------------------------------------------------------

/// Fills the `{{variable}}` and `{{variable | filter}}` placeholders of `input`. `\{{` writes a literal
/// `{{`, and braces that are not a placeholder (`{{ .Name }}`, `${{ matrix.os }}`) stay as they are.
fn render(input: &str, ctx: &Answers, origin: &str) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(open) = rest.find("{{") {
        if rest[..open].ends_with('\\') {
            out.push_str(&rest[..open - 1]);
            out.push_str("{{");
            rest = &rest[open + 2..];
            continue;
        }
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let placeholder = after.find("}}").and_then(|close| parse_placeholder(&after[..close]).map(|p| (close, p)));
        match placeholder {
            Some((close, (key, filters))) => {
                let Some(value) = ctx.get(&key) else {
                    let known: Vec<&str> = ctx.0.keys().map(String::as_str).collect();
                    return Err(kargo_util::help::unknown(
                        format!("{origin}: the placeholder `{{{{{key}}}}}` uses an unknown variable"),
                        &key,
                        &known,
                    ));
                };
                let mut text = value.text();
                for filter in filters {
                    text = apply_filter(&filter, &text, origin)?;
                }
                out.push_str(&text);
                rest = &after[close + 2..];
            }
            None => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// The variable and filters of the text between `{{` and `}}`, if it is one.
fn parse_placeholder(inner: &str) -> Option<(String, Vec<String>)> {
    let mut parts = inner.split('|').map(str::trim);
    let key = parts.next()?;
    if !is_variable_name(key) {
        return None;
    }
    let filters: Vec<String> = parts.map(str::to_owned).collect();
    if filters.iter().any(|f| f.is_empty() || !f.chars().all(|c| c.is_ascii_lowercase())) {
        return None;
    }
    Some((key.to_owned(), filters))
}

fn apply_filter(filter: &str, text: &str, origin: &str) -> Result<String> {
    Ok(match filter {
        "lower" => text.to_lowercase(),
        "upper" => text.to_uppercase(),
        "pascal" => words(text).iter().map(|w| capitalize(w)).collect(),
        "camel" => words(text).iter().enumerate().map(|(i, w)| if i == 0 { w.to_lowercase() } else { capitalize(w) }).collect(),
        "snake" => words(text).iter().map(|w| w.to_lowercase()).collect::<Vec<_>>().join("_"),
        "kebab" => words(text).iter().map(|w| w.to_lowercase()).collect::<Vec<_>>().join("-"),
        "path" => text.replace('.', "/"),
        "package" => text.split('.').map(package_segment).collect::<Vec<_>>().join("."),
        other => {
            return Err(kargo_util::help::unknown(
                format!("{origin}: unknown filter `{other}` (filters: {})", FILTERS.join(", ")),
                other,
                FILTERS,
            ));
        }
    })
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect()).unwrap_or_default()
}

/// The words of a name: split at anything that is not a letter or digit, and where a lowercase letter
/// or digit is followed by an uppercase one (`myApp`, `my-app` and `my_app` are `my` and `App`).
fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut after_lower = false;
    for c in text.chars() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            after_lower = false;
            continue;
        }
        if c.is_uppercase() && after_lower {
            words.push(std::mem::take(&mut current));
        }
        after_lower = c.is_lowercase() || c.is_ascii_digit();
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

// ---------------------------------------------------------------------------------------------------
// Where templates come from
// ---------------------------------------------------------------------------------------------------

/// A place to get a template from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Path(PathBuf),
    Git { url: String, rev: Option<String> },
}

/// A source, and the directory in it that holds the template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub source: Source,
    pub subdir: Option<String>,
}

/// A template of `templates.toml`.
#[derive(Debug, Clone)]
pub struct RegistryEntry {
    pub name: String,
    pub description: Option<String>,
    pub located: Located,
}

impl RegistryEntry {
    /// Where the entry points, as written in `kargo template list`.
    pub fn describe(&self) -> String {
        let mut text = match &self.located.source {
            Source::Path(path) => path.display().to_string(),
            Source::Git { url, rev: Some(rev) } => format!("{url}#{rev}"),
            Source::Git { url, rev: None } => url.clone(),
        };
        if let Some(subdir) = &self.located.subdir {
            text.push_str(&format!(" ({subdir})"));
        }
        text
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegistry {
    #[serde(default)]
    templates: toml::Table,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegistryEntry {
    git: Option<String>,
    rev: Option<String>,
    path: Option<String>,
    subdir: Option<String>,
    description: Option<String>,
}

/// The short names in `templates.toml` of the Kargo home.
#[derive(Debug, Default)]
pub struct Registry {
    pub file: PathBuf,
    pub entries: Vec<RegistryEntry>,
}

impl Registry {
    pub fn load(config: &Config) -> Result<Self> {
        let file = config.home().join(REGISTRY_FILE);
        if !file.is_file() {
            return Ok(Self { file, entries: Vec::new() });
        }
        let text = std::fs::read_to_string(&file).with_context(|| format!("failed to read {}", file.display()))?;
        Self::parse(&text, &file).with_context(|| format!("failed to parse {}", file.display()))
    }

    fn parse(text: &str, file: &Path) -> Result<Self> {
        let raw: RawRegistry = toml::from_str(text)?;
        let base = file.parent().unwrap_or(Path::new("."));
        let mut entries = Vec::new();
        for (name, value) in raw.templates {
            let entry = match value {
                toml::Value::String(spec) => {
                    let located = literal(&spec, base).with_context(|| {
                        format!("the template `{name}` is `{spec}`, which is neither a git URL nor a path (starting with `.`, `~` or `/`)")
                    })?;
                    RegistryEntry { name, description: None, located }
                }
                table @ toml::Value::Table(_) => {
                    let entry: RawRegistryEntry = table.try_into().with_context(|| format!("invalid template `{name}`"))?;
                    let source = match (entry.git, entry.path) {
                        (Some(url), None) => Source::Git { url, rev: entry.rev },
                        (None, Some(path)) if entry.rev.is_none() => Source::Path(expand_path(&path, base)),
                        (None, Some(_)) => bail!("the template `{name}` has a `rev` but is a path; `rev` only applies to `git`"),
                        _ => bail!("the template `{name}` needs either `git` or `path`"),
                    };
                    RegistryEntry { name, description: entry.description, located: Located { source, subdir: entry.subdir } }
                }
                other => bail!("the template `{name}` must be a string or a table, not `{other}`"),
            };
            entries.push(entry);
        }
        Ok(Self { file: file.to_path_buf(), entries })
    }

    fn find(&self, name: &str) -> Option<&RegistryEntry> {
        self.entries.iter().find(|e| e.name == name)
    }
}

fn expand_path(path: &str, base: &Path) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    base.join(path)
}

/// A source written out in full: a git URL (with an optional `#revision`) or a path that starts with `.`, `~`
/// or is absolute. `None` for anything else, which may be a short name.
fn literal(spec: &str, base: &Path) -> Option<Located> {
    let is_git = spec.contains("://") || spec.starts_with("git@") || spec.split('#').next().is_some_and(|u| u.ends_with(".git"));
    let source = if is_git {
        let (url, rev) = match spec.rsplit_once('#') {
            Some((url, rev)) if !rev.is_empty() => (url, Some(rev.to_owned())),
            _ => (spec.trim_end_matches('#'), None),
        };
        Source::Git { url: url.to_owned(), rev }
    } else if spec.starts_with('.') || spec.starts_with('~') || Path::new(spec).is_absolute() {
        Source::Path(expand_path(spec, base))
    } else {
        return None;
    };
    Some(Located { source, subdir: None })
}

/// Finds what `spec` (the argument of `--template`) stands for: a git URL, a path, a name from
/// `templates.toml`, or a directory of that name. `rev` (`--rev`) replaces the revision of a git source.
pub fn locate(config: &Config, spec: &str, rev: Option<&str>) -> Result<Located> {
    let cwd = std::env::current_dir().context("failed to read the current directory")?;
    let registry = Registry::load(config)?;
    let mut located = if let Some(located) = literal(spec, &cwd) {
        located
    } else if let Some(entry) = registry.find(spec) {
        entry.located.clone()
    } else if Path::new(spec).is_dir() {
        Located { source: Source::Path(cwd.join(spec)), subdir: None }
    } else {
        let names: Vec<&str> = registry.entries.iter().map(|e| e.name.as_str()).collect();
        let hint = if names.is_empty() {
            format!("pass a git URL or a path, or add a short name to {}", registry.file.display())
        } else {
            format!("the short names are {}", names.join(", "))
        };
        return Err(kargo_util::help::unknown(format!("no template `{spec}`: {hint}"), spec, &names));
    };
    if let Some(rev) = rev {
        match &mut located.source {
            Source::Git { rev: slot, .. } => *slot = Some(rev.to_owned()),
            Source::Path(_) => bail!("--rev only applies to a template from a git repository, and `{spec}` is a directory"),
        }
    }
    Ok(located)
}

/// The directory of the template of `located`, cloning or updating the repository of a git source in the Kargo
/// home first (not when offline). Returns it with a note on what was used, such as the commit.
pub fn fetch(config: &Config, located: &Located) -> Result<(PathBuf, String)> {
    let (root, label) = match &located.source {
        Source::Path(path) => {
            if !path.is_dir() {
                bail!("the template directory `{}` does not exist", path.display());
            }
            (path.clone(), path.display().to_string())
        }
        Source::Git { url, rev } => {
            let (dir, commit) = checkout(config, url, rev.as_deref())?;
            (dir, format!("{url} at {}", &commit[..commit.len().min(12)]))
        }
    };
    let root = match &located.subdir {
        Some(subdir) => {
            let relative = Path::new(subdir);
            if relative.is_absolute() || relative.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
                bail!("the subdirectory `{subdir}` of the template must be a relative path inside the repository");
            }
            root.join(relative)
        }
        None => root,
    };
    Ok((root, label))
}

fn git(dir: Option<&Path>, args: &[&str]) -> Result<String> {
    let git = kargo_util::process::find_on_path("git")
        .context("git is needed for a template from a git repository; install it, or use a template in a directory")?;
    let mut cmd = std::process::Command::new(git);
    cmd.args(args).stdin(std::process::Stdio::null());
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    let output = cmd.output().context("failed to run git")?;
    if !output.status.success() {
        bail!("`git {}` failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// A clone of `url` under the Kargo home with `rev` (a branch, tag or commit; the default branch when `None`)
/// checked out. Returns the clone and the commit.
fn checkout(config: &Config, url: &str, rev: Option<&str>) -> Result<(PathBuf, String)> {
    let shell = config.shell();
    let slug: String = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("template")
        .trim_end_matches(".git")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let id = kargo_util::hash::sha256_bytes(url.as_bytes());
    let dir = config.home().join("templates").join("git").join(format!("{slug}-{}", &id[..12]));
    if dir.exists() && !dir.join(".git").exists() {
        kargo_util::fs::remove_dir_all_if_exists(&dir)?;
    }
    if !dir.exists() {
        if config.offline() {
            bail!("the template repository `{url}` is not downloaded and --offline is set");
        }
        shell.status("Cloning", url);
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let target = dir.to_string_lossy().into_owned();
        git(None, &["-c", "core.autocrlf=false", "clone", "--quiet", "--", url, target.as_str()])?;
    } else if !config.offline() {
        shell.status("Updating", url);
        if let Err(e) = git(Some(&dir), &["fetch", "--quiet", "--tags", "--force", "origin"]) {
            shell.warn(format!("could not update the template repository, using the copy downloaded earlier: {e}"));
        }
    }
    let candidates: Vec<String> = match rev {
        Some(rev) => vec![format!("origin/{rev}"), rev.to_owned()],
        None => vec!["origin/HEAD".to_owned(), "HEAD".to_owned()],
    };
    let commit = candidates
        .iter()
        .find_map(|c| {
            let spec = format!("{c}^{{commit}}");
            git(Some(&dir), &["rev-parse", "--verify", "--quiet", spec.as_str()]).ok().filter(|s| !s.is_empty())
        })
        .with_context(|| match rev {
            Some(rev) => format!("`{rev}` is not a branch, tag or commit of `{url}`"),
            None => format!("`{url}` has no commits"),
        })?;
    git(Some(&dir), &["checkout", "--quiet", "--detach", "--force", commit.as_str()])?;
    Ok((dir, commit))
}

// ---------------------------------------------------------------------------------------------------
// Creating a project
// ---------------------------------------------------------------------------------------------------

/// Everything `kargo new --template` and `kargo init --template` need.
#[derive(Debug, Clone)]
pub struct Request {
    /// The `--template` argument.
    pub source: String,
    pub rev: Option<String>,
    pub defines: Vec<String>,
    pub defaults: bool,
    pub allow_scripts: bool,
    pub name: String,
    pub group: Option<String>,
    pub kotlin: Option<String>,
    /// Run the `git-init` step (`--no-vcs` and workspace members turn it off).
    pub vcs: bool,
}

/// What was created.
#[derive(Debug)]
pub struct Created {
    pub files: Vec<PathBuf>,
    /// What the template is called, for the status line.
    pub template: String,
    /// The generated `kargo.toml` is a package's (rather than a workspace's), or there is none.
    pub package: bool,
    pub message: Option<String>,
}

/// Creates a project in `dir` from the template of `request`. Everything is checked before the first file is
/// written: the answers, the files, the `kargo.toml`, and that the steps are allowed. `workspace` is the
/// workspace `dir` joins, whose settings the generated `kargo.toml` may inherit.
pub fn create(config: &Config, dir: &Path, workspace: Option<&Path>, request: &Request) -> Result<Created> {
    let shell = config.shell();
    let located = locate(config, &request.source, request.rev.as_deref())?;
    let (root, label) = fetch(config, &located)?;
    let template = Template::load(&root)?;
    let title = template.name.clone().unwrap_or_else(|| request.source.clone());
    shell.status("Using", format!("template `{title}` from {label}"));

    let inputs = Inputs {
        name: request.name.clone(),
        group: request.group.clone(),
        kotlin: request.kotlin.clone(),
        defines: parse_defines(&request.defines)?,
        defaults: request.defaults,
    };
    let ctx = template.resolve(shell, &inputs)?;
    let outputs = template.plan(&ctx)?;
    let manifest = outputs.iter().find(|o| o.path == Path::new(MANIFEST_NAME));
    let package = match manifest {
        Some(manifest) => check_manifest(manifest, dir, workspace)?,
        None => true,
    };
    if manifest.is_some() && dir.join(MANIFEST_NAME).exists() {
        bail!("`{}` already contains a {MANIFEST_NAME}", dir.display());
    }
    let steps = template.steps(&ctx)?;
    let commands: Vec<String> =
        steps.iter().filter_map(|s| if let PlannedStep::Run(argv) = s { Some(argv.join(" ")) } else { None }).collect();
    if !commands.is_empty() && !request.allow_scripts {
        let list: String = commands.iter().map(|c| format!("\n  {c}")).collect();
        bail!("the template runs commands after creating the project:{list}\nread them, and pass --allow-scripts to run them");
    }

    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let files = write(dir, &outputs)?;
    for step in steps {
        match step {
            PlannedStep::GitInit if request.vcs => super::new::git_init(dir),
            PlannedStep::GitInit => {}
            PlannedStep::Fetch => {
                let exe = std::env::current_exe().context("failed to find the kargo executable")?;
                let mut cmd = std::process::Command::new(exe);
                cmd.arg("fetch").current_dir(dir);
                if config.offline() {
                    cmd.arg("--offline");
                }
                shell.status("Running", "`kargo fetch`");
                kargo_util::process::run(shell, &mut cmd, "`kargo fetch`")?;
            }
            PlannedStep::Run(argv) => {
                shell.status("Running", format!("`{}`", argv.join(" ")));
                let program = kargo_util::process::find_on_path(&argv[0]).unwrap_or_else(|| PathBuf::from(&argv[0]));
                let mut cmd = std::process::Command::new(program);
                cmd.args(&argv[1..]).current_dir(dir);
                kargo_util::process::run(shell, &mut cmd, &format!("`{}`", argv.join(" ")))?;
            }
        }
    }
    Ok(Created { files, template: title, package, message: template.message(&ctx)? })
}

/// Parses the generated `kargo.toml`, so a template that writes an invalid one fails before anything is created.
/// Returns whether it is a package's manifest (rather than a workspace's).
fn check_manifest(manifest: &Output, dir: &Path, workspace: Option<&Path>) -> Result<bool> {
    let text = std::str::from_utf8(&manifest.contents).context("the generated kargo.toml is not UTF-8 text")?;
    let result = (|| -> Result<bool> {
        let table: toml::Table = toml::from_str(text)?;
        if table.contains_key("workspace") {
            kargo_manifest::WorkspaceManifest::parse(text, dir)?;
            return Ok(false);
        }
        let inherited = workspace.map(|root| kargo_manifest::WorkspaceManifest::load(&root.join(MANIFEST_NAME))).transpose()?;
        kargo_manifest::Manifest::parse_in(text, inherited.as_ref())?;
        Ok(true)
    })();
    result.with_context(|| format!("the template generates an invalid {MANIFEST_NAME} (from `{}`)", manifest.source))
}

/// Writes `outputs` into `dir`. A file that exists already stays as it is, so `kargo init` keeps the user's files.
fn write(dir: &Path, outputs: &[Output]) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for output in outputs {
        let path = dir.join(&output.path);
        if path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, &output.contents).with_context(|| format!("failed to write {}", path.display()))?;
        #[cfg(unix)]
        if output.executable {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)?.permissions().mode();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode | 0o111))?;
        }
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::Verbosity;

    fn shell() -> Shell {
        Shell::new(Verbosity::Quiet)
    }

    fn context(pairs: &[(&str, &str)]) -> Answers {
        Answers(pairs.iter().map(|(k, v)| ((*k).to_owned(), Value::Text((*v).to_owned()))).collect())
    }

    fn template_in(dir: &Path, manifest: &str, files: &[(&str, &str)]) -> Template {
        std::fs::write(dir.join(TEMPLATE_MANIFEST), manifest).unwrap();
        for (path, contents) in files {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        Template::load(dir).unwrap()
    }

    fn inputs(name: &str) -> Inputs {
        Inputs { name: name.to_owned(), defaults: true, ..Inputs::default() }
    }

    #[test]
    fn placeholders_are_filled_in() {
        let ctx = context(&[("name", "my-app"), ("package", "com.example.myapp")]);
        assert_eq!(render("{{name}} / {{ package }}", &ctx, "t").unwrap(), "my-app / com.example.myapp");
        assert_eq!(render("{{name | pascal}} {{name | snake}} {{name | upper}}", &ctx, "t").unwrap(), "MyApp my_app MY-APP");
        assert_eq!(render("{{package | path}}", &ctx, "t").unwrap(), "com/example/myapp");
        assert_eq!(render("{{name | camel}}", &context(&[("name", "My_big-app")]), "t").unwrap(), "myBigApp");
    }

    #[test]
    fn braces_that_are_not_placeholders_stay() {
        let ctx = context(&[("name", "x")]);
        for text in ["${{ matrix.os }}", "{{ .Name }}", "{{Name}}", "{{ unterminated", "a {{ b } c }}"] {
            assert_eq!(render(text, &ctx, "t").unwrap(), text);
        }
        assert_eq!(render(r"\{{name}} and {{name}}", &ctx, "t").unwrap(), "{{name}} and x");
    }

    #[test]
    fn unknown_variables_and_filters_are_errors() {
        let ctx = context(&[("name", "x")]);
        let err = render("{{nmae}}", &ctx, "src/Main.kt").unwrap_err().to_string();
        assert!(err.contains("src/Main.kt") && err.contains("unknown variable"), "{err}");
        assert!(render("{{name | shout}}", &ctx, "t").unwrap_err().to_string().contains("unknown filter"));
    }

    #[test]
    fn conditions() {
        let known = |k: &str| matches!(k, "docker" | "ci");
        let ctx = Answers(BTreeMap::from([("docker".to_owned(), Value::Bool(true)), ("ci".to_owned(), Value::Text("github".to_owned()))]));
        let holds = |expr: &str| Condition::parse(expr, &known).unwrap().holds(&ctx);
        assert!(holds("docker") && !holds("!docker"));
        assert!(holds("ci == \"github\"") && holds("ci != gitlab") && !holds("ci == gitlab"));
        assert!(Condition::parse("nope", &known).is_err());
        assert!(Condition::parse("a b", &known).is_err());
    }

    #[test]
    fn rendered_paths_stay_inside_the_project() {
        let ctx = context(&[("package_path", "com/example/app"), ("bad", "../x"), ("empty", "")]);
        assert_eq!(rendered_path("src/{{package_path}}/Main.kt", &ctx).unwrap(), Some(PathBuf::from("src/com/example/app/Main.kt")));
        assert!(rendered_path("{{bad}}/Main.kt", &ctx).is_err());
        assert_eq!(rendered_path("{{empty}}/Main.kt", &ctx).unwrap(), None);
    }

    #[test]
    fn variables_resolve_from_defines_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let template = template_in(
            dir.path(),
            r#"
[variables.service]
default = "{{name}}-service"
[variables.docker]
default = true
[variables.db]
choices = ["postgres", "mysql"]
default = "postgres"
[variables.group]
default = "org.acme"
"#,
            &[],
        );
        let ctx = template.resolve(&shell(), &inputs("shop")).unwrap();
        assert_eq!(ctx.value("service").as_deref(), Some("shop-service"));
        assert_eq!(ctx.value("docker").as_deref(), Some("true"));
        assert_eq!(ctx.value("group").as_deref(), Some("org.acme"));
        assert_eq!(ctx.value("package").as_deref(), Some("org.acme.shop"));
        assert_eq!(ctx.value("package_path").as_deref(), Some("org/acme/shop"));
        assert_eq!(ctx.value("kotlin").as_deref(), Some(DEFAULT_KOTLIN_VERSION));

        let defines = parse_defines(&["db=mysql".into(), "docker=no".into(), "group=io.me".into()]).unwrap();
        let ctx = template.resolve(&shell(), &Inputs { defines, ..inputs("shop") }).unwrap();
        assert_eq!(ctx.value("db").as_deref(), Some("mysql"));
        assert_eq!(ctx.value("docker").as_deref(), Some("false"));
        assert_eq!(ctx.value("package").as_deref(), Some("io.me.shop"));

        // A flag beats the template's default.
        let ctx = template.resolve(&shell(), &Inputs { group: Some("net.flag".into()), ..inputs("shop") }).unwrap();
        assert_eq!(ctx.value("group").as_deref(), Some("net.flag"));
    }

    #[test]
    fn bad_answers_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let template = template_in(
            dir.path(),
            "[variables.db]\nchoices = [\"postgres\", \"mysql\"]\n[variables.docker]\ndefault = true\n[variables.owner]\nprompt = \"Owner\"\n",
            &[],
        );
        let with = |define: &str| {
            let defines = parse_defines(&[define.to_owned(), "db=mysql".into(), "owner=me".into()]).unwrap();
            template.resolve(&shell(), &Inputs { defines, ..inputs("x") })
        };
        assert!(with("docker=maybe").unwrap_err().to_string().contains("yes or no"));
        assert!(with("nothing=1").unwrap_err().to_string().contains("no variable `nothing`"));
        assert!(with("package=class").unwrap_err().to_string().contains("invalid package"));
        assert!(with("group=bad..group").unwrap_err().to_string().contains("invalid group"));
        // `db` and `owner` have no default.
        let err = template.resolve(&shell(), &inputs("x")).unwrap_err().to_string();
        assert!(err.contains("needs a value") && err.contains("--define"), "{err}");
        assert!(template.resolve(&shell(), &inputs("9lives")).unwrap_err().to_string().contains("must start with a letter"));
        assert!(parse_defines(&["novalue".into()]).is_err());
    }

    #[test]
    fn invalid_templates_are_rejected() {
        let check = |manifest: &str| {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(TEMPLATE_MANIFEST), manifest).unwrap();
            Template::load(dir.path()).unwrap_err().to_string() + &format!("{:?}", Template::load(dir.path()).unwrap_err())
        };
        assert!(check("[variables.name]\ndefault = \"x\"").contains("cannot be declared"));
        assert!(check("[variables.Bad]\ndefault = \"x\"").contains("invalid variable name"));
        assert!(check("[variables.db]\nchoices = [\"a\"]\ndefault = \"b\"").contains("not one of its choices"));
        assert!(check("[variables.x]\ndefault = 3").contains("must be a"));
        assert!(check("[[conditional]]\npath = \"a\"\nwhen = \"missing\"").contains("unknown variable"));
        assert!(check("[[steps]]\nbuiltin = \"nope\"").contains("unknown builtin"));
        assert!(check("[[steps]]\nrun = [\"a\"]\nbuiltin = \"fetch\"").contains("either"));
        assert!(check("unknown = 1").contains("unknown field"));
        let empty = tempfile::tempdir().unwrap();
        assert!(Template::load(empty.path()).unwrap_err().to_string().contains("not a template"));
    }

    #[test]
    fn files_are_rendered_and_selected() {
        let dir = tempfile::tempdir().unwrap();
        let template = template_in(
            dir.path(),
            r#"
[variables.docker]
default = false
[variables.ci]
choices = ["github", "none"]
default = "github"

[files]
exclude = ["notes.md", "*.bak"]
raw = ["workflows/**"]

[[conditional]]
path = "Dockerfile"
when = "docker"

[[conditional]]
path = "ci/"
when = "ci == github"
"#,
            &[
                ("src/{{package_path}}/Main.kt", "package {{package}}\n\nfun main() = println(\"{{name}}\")\n"),
                ("Dockerfile", "FROM {{kotlin}}\n"),
                ("ci/build.yml", "run: ${{ matrix.os }} {{name}}\n"),
                ("workflows/x.yml", "{{name}}\n"),
                ("notes.md", "internal"),
                ("old.bak", "x"),
                (".git/config", "x"),
            ],
        );
        std::fs::write(dir.path().join("logo.bin"), [0u8, 159, 146, 150]).unwrap();
        let plan = |template: &Template, ctx: &Answers| -> BTreeMap<String, String> {
            template
                .plan(ctx)
                .unwrap()
                .into_iter()
                .map(|o| (o.path.to_string_lossy().replace('\\', "/"), String::from_utf8_lossy(&o.contents).into_owned()))
                .collect()
        };
        let ctx = template.resolve(&shell(), &inputs("demo")).unwrap();
        let files = plan(&template, &ctx);
        assert_eq!(
            files.keys().map(String::as_str).collect::<Vec<_>>(),
            ["ci/build.yml", "logo.bin", "src/com/example/demo/Main.kt", "workflows/x.yml"]
        );
        assert_eq!(files["src/com/example/demo/Main.kt"], "package com.example.demo\n\nfun main() = println(\"demo\")\n");
        assert_eq!(files["ci/build.yml"], "run: ${{ matrix.os }} demo\n");
        assert_eq!(files["workflows/x.yml"], "{{name}}\n");

        let defines = parse_defines(&["docker=true".into(), "ci=none".into()]).unwrap();
        let ctx = template.resolve(&shell(), &Inputs { defines, ..inputs("demo") }).unwrap();
        let files = plan(&template, &ctx);
        assert!(files.contains_key("Dockerfile") && !files.contains_key("ci/build.yml"));
        assert_eq!(files["Dockerfile"], format!("FROM {DEFAULT_KOTLIN_VERSION}\n"));
    }

    #[test]
    fn include_limits_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let template = template_in(
            dir.path(),
            "[files]\ninclude = [\"src/\", \"kargo.toml\"]\n",
            &[("src/A.kt", "a"), ("kargo.toml", "b"), ("docs/x.md", "c")],
        );
        let ctx = template.resolve(&shell(), &inputs("demo")).unwrap();
        let paths: Vec<_> = template.plan(&ctx).unwrap().into_iter().map(|o| o.path).collect();
        assert_eq!(paths, [PathBuf::from("kargo.toml"), PathBuf::from("src/A.kt")]);
    }

    #[test]
    fn two_files_with_one_name_are_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let template = template_in(dir.path(), "", &[("{{name}}.txt", "a"), ("demo.txt", "b")]);
        let ctx = template.resolve(&shell(), &inputs("demo")).unwrap();
        assert!(template.plan(&ctx).unwrap_err().to_string().contains("written to"));
    }

    #[test]
    fn steps_follow_their_conditions() {
        let dir = tempfile::tempdir().unwrap();
        let template = template_in(
            dir.path(),
            r#"
[variables.docker]
default = false

[[steps]]
builtin = "git-init"

[[steps]]
builtin = "fetch"
when = "!docker"

[[steps]]
run = ["echo", "{{name}}"]
when = "docker"
"#,
            &[],
        );
        let ctx = template.resolve(&shell(), &inputs("demo")).unwrap();
        assert_eq!(template.steps(&ctx).unwrap(), [PlannedStep::GitInit, PlannedStep::Fetch]);
        let defines = parse_defines(&["docker=true".into()]).unwrap();
        let ctx = template.resolve(&shell(), &Inputs { defines, ..inputs("demo") }).unwrap();
        assert_eq!(template.steps(&ctx).unwrap(), [PlannedStep::GitInit, PlannedStep::Run(vec!["echo".into(), "demo".into()])]);
    }

    #[test]
    fn registry_maps_names_to_sources() {
        let file = Path::new("/home/me/.kargo/templates.toml");
        let registry = Registry::parse(
            r#"
[templates]
service = "https://example.com/acme/service.git#v2"
local = "../templates/local"
mono = { git = "https://example.com/acme/all.git", rev = "main", subdir = "templates/lib", description = "A library" }
dir = { path = "templates/dir" }
"#,
            file,
        )
        .unwrap();
        let by = |name: &str| registry.find(name).unwrap().located.clone();
        assert_eq!(by("service").source, Source::Git { url: "https://example.com/acme/service.git".into(), rev: Some("v2".into()) });
        assert_eq!(by("local").source, Source::Path(file.parent().unwrap().join("../templates/local")));
        assert_eq!(by("mono").subdir.as_deref(), Some("templates/lib"));
        assert_eq!(registry.find("mono").unwrap().description.as_deref(), Some("A library"));
        assert!(matches!(by("dir").source, Source::Path(_)));
        for bad in [
            "[templates]\nx = \"name-only\"",
            "[templates]\nx = { rev = \"a\" }",
            "[templates]\nx = { path = \"a\", rev = \"b\" }",
            "[templates]\nx = 3",
            "extra = 1",
        ] {
            assert!(Registry::parse(bad, file).is_err(), "{bad}");
        }
    }

    #[test]
    fn literal_sources() {
        let base = Path::new("/base");
        assert!(matches!(literal("https://github.com/a/b", base), Some(Located { source: Source::Git { rev: None, .. }, .. })));
        assert!(
            matches!(literal("git@github.com:a/b.git#dev", base), Some(Located { source: Source::Git { rev: Some(r), .. }, .. }) if r == "dev")
        );
        assert!(matches!(literal("./here", base), Some(Located { source: Source::Path(_), .. })));
        assert_eq!(literal("company/service", base), None);
    }
}
