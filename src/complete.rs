//! Shell completions: the `kargo completions <shell>` scripts and the dynamic candidates (tasks, plugins,
//! frameworks, installed toolchains, bins and examples) they ask the `kargo` binary for while the user types.
//!
//! The scripts are thin: on every <Tab> they run `COMPLETE=<shell> kargo -- <words>`, which
//! [`complete_if_requested`] answers from the clap definition plus the current project.

use std::io::Write;

use anyhow::{Context, Result};
use clap::{CommandFactory, ValueEnum};
use clap_complete::env::{EnvCompleter, Shells};
use clap_complete::{CompleteEnv, CompletionCandidate};

use kargo_ops::workspace::{Project, Workspace};
use kargo_util::config::Config;
use kargo_util::shell::{Shell, Verbosity};

use crate::cli::Cli;

/// The environment variable the completion scripts set when they call back into `kargo`.
const COMPLETE_ENV: &str = "COMPLETE";

/// Shells `kargo completions` can generate a script for.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ShellArg {
    Bash,
    Zsh,
    Fish,
    Powershell,
    Elvish,
}

impl ShellArg {
    fn completer(self) -> &'static dyn EnvCompleter {
        match self {
            ShellArg::Bash => &clap_complete::env::Bash,
            ShellArg::Zsh => &clap_complete::env::Zsh,
            ShellArg::Fish => &clap_complete::env::Fish,
            ShellArg::Powershell => &clap_complete::env::Powershell,
            ShellArg::Elvish => &clap_complete::env::Elvish,
        }
    }
}

/// Answers a completion request from a shell script and exits, if this process is one.
pub fn complete_if_requested() {
    CompleteEnv::with_factory(Cli::command).var(COMPLETE_ENV).shells(Shells::builtins()).complete();
}

/// Writes the completion script for `shell`.
pub fn write_script(shell: ShellArg, out: &mut dyn Write) -> Result<()> {
    // The script calls back into whatever `kargo` is on PATH, so it keeps working across upgrades.
    shell.completer().write_registration(COMPLETE_ENV, "kargo", "kargo", "kargo", out).context("failed to write the completion script")
}

fn workspace() -> Option<Workspace> {
    let cwd = std::env::current_dir().ok()?;
    Workspace::discover(&cwd, None).ok()
}

fn config() -> Option<Config> {
    Config::from_env(Shell::new(Verbosity::Quiet)).ok()
}

/// The `[tasks]` of the package in the current directory (of every member at the root of a
/// workspace) and the `[workspace.tasks]`.
pub fn tasks() -> Vec<CompletionCandidate> {
    let Some(project) = std::env::current_dir().ok().and_then(|cwd| Project::discover(&cwd, None).ok()) else { return Vec::new() };
    let packages: Vec<usize> = match project.current() {
        Some(index) => vec![index],
        None => project.members().to_vec(),
    };
    let mut tasks: Vec<&kargo_manifest::Task> = packages.iter().flat_map(|&i| project.package(i).manifest().tasks.values()).collect();
    tasks.extend(project.workspace_manifest().into_iter().flat_map(|w| w.tasks.values()));
    let mut seen: Vec<&str> = Vec::new();
    tasks
        .into_iter()
        .filter(|t| {
            let new = !seen.contains(&t.name.as_str());
            seen.push(&t.name);
            new
        })
        .map(|t| {
            let help = t.description.clone().unwrap_or_else(|| t.platform_run().join(" && "));
            CompletionCandidate::new(&t.name).help(Some(help.into())).tag(Some("Tasks".into()))
        })
        .collect()
}

/// The `[[bin]]` programs of the project in the current directory, for `--bin`.
pub fn bins() -> Vec<CompletionCandidate> {
    let Some(ws) = workspace() else { return Vec::new() };
    ws.manifest()
        .bins
        .iter()
        .map(|b| {
            let help = b.main.clone().or_else(|| b.path.as_ref().map(|p| p.display().to_string())).or_else(|| b.entry_point.clone());
            CompletionCandidate::new(&b.name).help(help.map(Into::into))
        })
        .collect()
}

/// The programs `kargo install` installed, for `kargo uninstall` and `kargo install --force`.
pub fn installed_programs() -> Vec<CompletionCandidate> {
    let Some(config) = config() else { return Vec::new() };
    kargo_ops::ops::install::installed(&kargo_ops::ops::install::Layout::new(&config, None))
        .unwrap_or_default()
        .into_iter()
        .map(|tool| CompletionCandidate::new(&tool.name).help(Some(format!("v{}", tool.version).into())))
        .collect()
}

/// The `examples/<name>.kt` programs of the project in the current directory, for `--example`.
pub fn examples() -> Vec<CompletionCandidate> {
    let Some(ws) = workspace() else { return Vec::new() };
    ws.examples().into_iter().map(CompletionCandidate::new).collect()
}

/// Everything `kargo <name>` can run besides the built-in commands: project tasks and `kargo-*` executables.
pub fn external_subcommands() -> Vec<CompletionCandidate> {
    let mut out = tasks();
    if let Some(config) = config() {
        for (name, path) in kargo_ops::plugins::list_subcommands(&config) {
            if !out.iter().any(|c| c.get_value() == name.as_str()) {
                out.push(CompletionCandidate::new(name).help(Some(path.display().to_string().into())).tag(Some("Plugins".into())));
            }
        }
    }
    out
}

/// The built-in framework names for `kargo new --with` and `kargo add --framework`.
pub fn frameworks() -> Vec<CompletionCandidate> {
    kargo_ops::builtins::FRAMEWORK_NAMES.iter().map(CompletionCandidate::new).collect()
}

/// The framework names and presets for `kargo new --with`.
pub fn new_frameworks() -> Vec<CompletionCandidate> {
    kargo_ops::builtins::FRAMEWORK_NAMES.iter().chain(kargo_ops::ops::new::PRESETS).map(CompletionCandidate::new).collect()
}

/// Installed JDK releases and majors and Kotlin/Native versions, for `kargo toolchain remove`.
pub fn installed_toolchains() -> Vec<CompletionCandidate> {
    let Some(config) = config() else { return Vec::new() };
    let mut out: Vec<CompletionCandidate> = Vec::new();
    let jdks = kargo_toolchain::jdks::installed(&config).unwrap_or_default();
    let mut majors: Vec<u32> = jdks.iter().map(|j| j.major).collect();
    majors.dedup();
    for major in majors {
        out.push(CompletionCandidate::new(major.to_string()).help(Some("JDK (all releases)".into())));
    }
    for jdk in jdks {
        out.push(CompletionCandidate::new(jdk.release).help(Some("JDK".into())));
    }
    for native in kargo_toolchain::NativeToolchain::installed(&config).unwrap_or_default() {
        out.push(CompletionCandidate::new(native.version).help(Some("Kotlin/Native".into())));
    }
    for gradle in kargo_toolchain::GradleDistribution::installed(&config).unwrap_or_default() {
        out.push(CompletionCandidate::new(gradle.version).help(Some("Gradle".into())));
    }
    for server in kargo_toolchain::LanguageServer::installed(&config).unwrap_or_default() {
        out.push(CompletionCandidate::new(server.version).help(Some(server.kind.title().into())));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_call_back_into_kargo() {
        for shell in ShellArg::value_variants() {
            let mut buf = Vec::new();
            write_script(*shell, &mut buf).unwrap();
            let script = String::from_utf8(buf).unwrap();
            assert!(script.contains("COMPLETE"), "{shell:?}: {script}");
            assert!(script.contains("kargo"), "{shell:?}: {script}");
        }
    }
}
