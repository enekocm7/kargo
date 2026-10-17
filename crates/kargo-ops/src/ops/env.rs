//! `kargo env`: the environment `kargo run` and `kargo test` start programs with, and where each
//! variable comes from. Values stay hidden unless asked for.

use anyhow::Result;

use crate::env::{EnvOptions, resolve};
use crate::workspace::{ProfileKind, Workspace};
use kargo_util::shell::Shell;

/// What is printed in place of a value.
const HIDDEN: &str = "********";

/// Prints each variable of the package's environment with its source, then fails when a required
/// variable is not set.
pub fn show(shell: &Shell, ws: &Workspace, profile: ProfileKind, opts: &EnvOptions, show_values: bool) -> Result<()> {
    let env = resolve(ws, profile.name(), opts)?;
    if env.vars.is_empty() && env.missing.is_empty() {
        shell.note("no variables are set; add them to [env] in kargo.toml or to a .env file");
        return Ok(());
    }

    let mut rows: Vec<[String; 3]> = Vec::new();
    for (name, var) in &env.vars {
        let value = if show_values { var.value.clone() } else { HIDDEN.to_owned() };
        rows.push([name.clone(), var.source.describe(), value]);
    }
    for (name, source) in &env.missing {
        rows.push([name.clone(), source.describe(), "missing (required)".to_owned()]);
    }
    let width = |column: usize| rows.iter().map(|row| row[column].len()).max().unwrap_or(0);
    let (name_width, source_width) = (width(0), width(1));
    for [name, source, value] in &rows {
        println!("{name:<name_width$}  {source:<source_width$}  {value}");
    }

    if !show_values {
        shell.note("values are hidden; pass --show-values to print them");
    }
    env.ensure_set().map(|_| ())
}
