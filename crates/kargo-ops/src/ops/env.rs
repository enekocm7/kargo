//! `kargo env`: the environment `kargo run` and `kargo test` start programs with, and where each
//! variable comes from. Values stay hidden unless asked for. `--check` fails on what a CI job must
//! fix: required variables that are unset, and keys of `.env.example` that nothing sets.

use anyhow::{Result, bail};

use crate::env::{EnvOptions, ProjectEnv, example_keys, resolve};
use crate::warnings;
use crate::workspace::{ProfileKind, Project};
use kargo_util::shell::Shell;

/// What is printed in place of a value.
const HIDDEN: &str = "********";

/// Prints each variable of the package's environment with its source. With `check`, fails when a
/// required variable is unset or a key of `.env.example` is neither set nor declared.
pub fn show(
    shell: &Shell,
    project: &Project,
    package: usize,
    profile: ProfileKind,
    opts: &EnvOptions,
    show_values: bool,
    check: bool,
) -> Result<()> {
    let ws = project.package(package);
    let env = resolve(ws, profile.name(), opts)?;
    warnings::emit(shell, project, ws, env.tracked_secret_warnings(ws.root()));

    let mut rows: Vec<[String; 3]> = Vec::new();
    for (name, var) in &env.vars {
        let value = if show_values { var.value.clone() } else { HIDDEN.to_owned() };
        rows.push([name.clone(), var.source.describe(), value]);
    }
    for (name, source) in &env.missing {
        rows.push([name.clone(), source.describe(), "missing (required)".to_owned()]);
    }
    if rows.is_empty() {
        shell.note("no variables are set; add them to [env] in kargo.toml or to a .env file");
    }
    let width = |column: usize| rows.iter().map(|row| row[column].len()).max().unwrap_or(0);
    let (name_width, source_width) = (width(0), width(1));
    for [name, source, value] in &rows {
        println!("{name:<name_width$}  {source:<source_width$}  {value}");
    }
    if !show_values && !rows.is_empty() {
        shell.note("values are hidden; pass --show-values to print them");
    }

    if check {
        check_env(ws.root(), &env)?;
    }
    Ok(())
}

/// Fails listing every problem: required variables that are unset, and keys of `.env.example`
/// that nothing sets. A package without `.env.example` has no such keys to check.
fn check_env(root: &std::path::Path, env: &ProjectEnv) -> Result<()> {
    let mut problems: Vec<String> =
        env.missing.iter().map(|(name, source)| format!("`{name}` is required but not set (declared in {})", source.describe())).collect();
    for key in example_keys(root)?.unwrap_or_default() {
        let reported = env.missing.iter().any(|(name, _)| *name == key);
        if !reported && !env.vars.contains_key(&key) && std::env::var_os(&key).is_none() {
            problems.push(format!("`{key}` is listed in .env.example but not set"));
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = problems.iter().map(|problem| format!("  {problem}")).collect();
    bail!("the environment is incomplete:\n{}", list.join("\n"))
}
