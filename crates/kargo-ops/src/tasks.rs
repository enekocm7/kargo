//! User-defined tasks from `[tasks]`: shell commands or Kotlin scripts with dependencies that
//! can also attach to build lifecycle hooks.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::script::{self, Source, Surroundings};
use kargo_manifest::{Hook, Task, TaskShell};
use kargo_util::config::Config;
use kargo_util::process;
use kargo_util::shell::Shell;

/// Orders `name` and its transitive dependencies so each runs after its prerequisites.
pub fn plan<'a>(tasks: &'a BTreeMap<String, Task>, name: &str) -> Result<Vec<&'a Task>> {
    fn visit<'a>(
        tasks: &'a BTreeMap<String, Task>,
        name: &str,
        visiting: &mut Vec<String>,
        done: &mut HashSet<String>,
        order: &mut Vec<&'a Task>,
    ) -> Result<()> {
        if done.contains(name) {
            return Ok(());
        }
        if let Some(pos) = visiting.iter().position(|n| n == name) {
            let mut cycle = visiting[pos..].to_vec();
            cycle.push(name.to_owned());
            bail!("task dependency cycle: {}", cycle.join(" -> "));
        }
        let Some(task) = tasks.get(name) else {
            let mut known: Vec<&str> = tasks.keys().map(String::as_str).collect();
            known.sort();
            if known.is_empty() {
                bail!("no task named `{name}`; the manifest defines no [tasks]");
            }
            bail!("no task named `{name}`; available tasks: {}", known.join(", "));
        };
        visiting.push(name.to_owned());
        for dep in &task.depends_on {
            visit(tasks, dep, visiting, done, order)?;
        }
        visiting.pop();
        done.insert(name.to_owned());
        order.push(task);
        Ok(())
    }

    let mut order = Vec::new();
    visit(tasks, name, &mut Vec::new(), &mut HashSet::new(), &mut order)?;
    Ok(order)
}

/// Builds a command that runs `script` in `shell`.
pub fn shell_command(shell: TaskShell, script: &str) -> Result<Command> {
    Ok(match shell {
        TaskShell::Cmd => {
            let mut cmd = Command::new("cmd");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                // Pass the script verbatim; cmd.exe does its own parsing.
                cmd.raw_arg("/D /S /C \"").raw_arg(script).raw_arg("\"");
            }
            #[cfg(not(windows))]
            cmd.args(["/D", "/S", "/C", script]);
            cmd
        }
        TaskShell::Sh | TaskShell::Bash => {
            let name = shell.as_str();
            let Some(program) = process::find_posix_shell(name) else {
                bail!("`{name}` was not found; install it (on Windows, Git for Windows includes it) or use `windows-run`");
            };
            let mut cmd = Command::new(program);
            cmd.arg("-c").arg(script);
            cmd
        }
        TaskShell::PowerShell | TaskShell::Pwsh => {
            let mut cmd = Command::new(shell.as_str());
            cmd.args(["-NoProfile", "-NonInteractive", "-Command"]).arg(powershell_script(shell, script));
            cmd
        }
    })
}

/// Wraps `script` so a failing command stops the task and its exit code becomes the task's, as with `sh`
/// and `cmd`. PowerShell keeps going after a native command fails, and `-Command` reports only the last
/// statement's status. `pwsh` 7.3+ turns a failing native command into an error with
/// `$PSNativeCommandUseErrorActionPreference`; Windows PowerShell 5.1 has no such switch, so it can only
/// report the code of the last native command.
fn powershell_script(shell: TaskShell, script: &str) -> String {
    let mut out = String::from(
        "$ErrorActionPreference = 'Stop'; $PSNativeCommandUseErrorActionPreference = $true; try {
",
    );
    out.push_str(script);
    out.push_str(
        "
} catch { [Console]::Error.WriteLine($_); if ($LASTEXITCODE) { exit $LASTEXITCODE } else { exit 1 } }",
    );
    if shell == TaskShell::PowerShell {
        out.push_str("; if ($LASTEXITCODE) { exit $LASTEXITCODE }");
    }
    out
}

/// Runs a single shell command with extra environment variables.
pub fn run_script(shell: &Shell, task_shell: TaskShell, script: &str, cwd: &Path, env: &[(String, String)], what: &str) -> Result<()> {
    let mut cmd = shell_command(task_shell, script).with_context(|| format!("cannot run {what}"))?;
    cmd.current_dir(cwd).envs(env.iter().map(|(k, v)| (k, v)));
    shell.verbose("Running", format!("`{script}`"));
    let status = process::status(shell, &mut cmd, what)?;
    if !status.success() {
        bail!("{what} failed: `{script}` exited with {}", process::describe_status(status));
    }
    Ok(())
}

/// Runs the Kotlin script at `script` (a task's or hook plugin's `script`, relative to `root`) as
/// `kargo script` does, in `cwd` with `env` added to the environment and `args` as its arguments.
pub fn run_kotlin_script(
    config: &Config,
    root: &Path,
    script: &Path,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    what: &str,
) -> Result<()> {
    let path = root.join(script);
    if !path.is_file() {
        bail!("{what}: script `{}` not found; `script` is relative to {}", script.display(), root.display());
    }
    config.shell().verbose("Running", format!("`kargo script {}`", script.display()));
    let around = Surroundings { cwd: Some(cwd.to_path_buf()), env: env.to_vec() };
    let code = script::run_in(config, &Source::File(path), args, &around).with_context(|| format!("cannot run {what}"))?;
    if code != 0 {
        bail!("{what} failed: `{}` ended with exit code {code}", script.display());
    }
    Ok(())
}

/// Runs a task (and its dependencies). `args` are appended to the last command
/// of the requested task, or are the arguments of its script.
pub fn run(
    config: &Config,
    root: &Path,
    tasks: &BTreeMap<String, Task>,
    name: &str,
    args: &[String],
    env: &[(String, String)],
) -> Result<()> {
    let shell = config.shell();
    let order = plan(tasks, name)?;
    for task in order {
        shell.status("Running", format!("task `{}`", task.name));
        let cwd = task.cwd.as_ref().map(|c| root.join(c)).unwrap_or_else(|| root.to_path_buf());
        let mut task_env: Vec<(String, String)> = env.to_vec();
        task_env.push(("KARGO_TASK".to_owned(), task.name.clone()));
        task_env.extend(task.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        if let Some(script) = &task.script {
            let args = if task.name == name { args } else { &[] };
            run_kotlin_script(config, root, script, args, &cwd, &task_env, &format!("task `{}`", task.name))?;
            continue;
        }
        let task_shell = task.shell.unwrap_or_else(TaskShell::platform_default);
        let commands = task.platform_run();
        let last = commands.len().saturating_sub(1);
        for (i, script) in commands.iter().enumerate() {
            let script = if task.name == name && i == last && !args.is_empty() {
                format!("{script} {}", args.iter().map(|a| quote_for_shell(task_shell, a)).collect::<Vec<_>>().join(" "))
            } else {
                script.clone()
            };
            run_script(shell, task_shell, &script, &cwd, &task_env, &format!("task `{}`", task.name))?;
        }
    }
    Ok(())
}

/// Tasks attached to a lifecycle hook, in name order.
pub fn tasks_for_hook(tasks: &BTreeMap<String, Task>, hook: Hook) -> Vec<&Task> {
    tasks.values().filter(|t| t.hooks.contains(&hook)).collect()
}

fn quote_for_shell(shell: TaskShell, arg: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c);
    if shell == TaskShell::Cmd && arg.contains('%') {
        return quote_percents_for_cmd(arg);
    }
    if !arg.is_empty() && arg.chars().all(safe) {
        return arg.to_owned();
    }
    match shell {
        TaskShell::Cmd => format!("\"{}\"", arg.replace('"', "\"\"")),
        TaskShell::PowerShell | TaskShell::Pwsh => format!("'{}'", arg.replace('\'', "''")),
        TaskShell::Sh | TaskShell::Bash => format!("'{}'", arg.replace('\'', "'\\''")),
    }
}

/// Quotes an argument with `%` for `cmd`, which expands `%NAME%` even inside quotes. A caret right after
/// the `%` (outside quotes, where it is removed afterwards) keeps cmd from finding a variable name between
/// two percent signs, so the text reaches the program as written.
fn quote_percents_for_cmd(arg: &str) -> String {
    let pieces: Vec<String> = arg
        .split('%')
        .map(|piece| {
            let bare = piece.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@".contains(c));
            if piece.is_empty() || bare { piece.to_owned() } else { format!("\"{}\"", piece.replace('"', "\"\"")) }
        })
        .collect();
    pieces.join("^%")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_manifest::Manifest;

    fn quiet() -> Config {
        Config::new(std::env::temp_dir().join("kargo-tasks-tests"), Shell::new(kargo_util::shell::Verbosity::Quiet))
    }

    fn tasks(toml: &str) -> BTreeMap<String, Task> {
        Manifest::parse(&format!("[package]\nname = \"t\"\nversion = \"1\"\n[tasks]\n{toml}")).unwrap().tasks
    }

    #[test]
    fn plans_in_dependency_order() {
        let t = tasks(
            "a = { run = \"echo a\", depends-on = [\"b\", \"c\"] }\nb = { run = \"echo b\", depends-on = [\"c\"] }\nc = \"echo c\"\n",
        );
        let order: Vec<_> = plan(&t, "a").unwrap().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(order, ["c", "b", "a"]);
    }

    #[test]
    fn detects_cycles_and_unknown_tasks() {
        let t = tasks("a = { run = \"x\", depends-on = [\"b\"] }\nb = { run = \"y\", depends-on = [\"a\"] }\n");
        let err = plan(&t, "a").unwrap_err().to_string();
        assert!(err.contains("cycle") && err.contains("a -> b -> a"), "{err}");
        let err = plan(&t, "zzz").unwrap_err().to_string();
        assert!(err.contains("available tasks: a, b"), "{err}");
        assert!(plan(&BTreeMap::new(), "x").unwrap_err().to_string().contains("no [tasks]"));
    }

    #[test]
    fn runs_commands_with_env_and_args() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.txt");
        let script = if cfg!(windows) { "echo %GREETING% %KARGO_TASK%> out.txt" } else { "echo $GREETING $KARGO_TASK > out.txt" };
        let t = tasks(&format!("hello = {{ run = '{script}', env = {{ GREETING = \"hi\" }} }}\n"));
        run(&quiet(), dir.path(), &t, "hello", &[], &[]).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap().trim(), "hi hello");

        let failing = tasks("bad = \"exit 3\"\n");
        let err = run(&quiet(), dir.path(), &failing, "bad", &[], &[]).unwrap_err();
        assert!(err.to_string().contains("exit code 3"), "{err}");
    }

    #[test]
    fn uses_windows_run_on_windows_and_run_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let t = tasks(
            "pick = { run = \"echo unix> out.txt\", windows-run = \"echo windows> out.txt\" }
",
        );
        run(&quiet(), dir.path(), &t, "pick", &[], &[]).unwrap();
        let expected = if cfg!(windows) { "windows" } else { "unix" };
        assert_eq!(std::fs::read_to_string(dir.path().join("out.txt")).unwrap().trim(), expected);
    }

    #[test]
    fn shell_sh_runs_posix_commands_on_every_platform() {
        if process::find_posix_shell("sh").is_none() {
            eprintln!("skipping: no sh available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let t = tasks(
            "posix = { run = 'echo \"$GREETING $KARGO_TASK\" > out.txt', shell = \"sh\", env = { GREETING = \"hi\" } }
",
        );
        run(&quiet(), dir.path(), &t, "posix", &["it's".into()], &[]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("out.txt")).unwrap().trim(), "hi posix it's");
    }

    #[test]
    fn appends_arguments_to_last_command() {
        let dir = tempfile::tempdir().unwrap();
        let t = tasks("say = \"echo\"\n");
        // Only checks that it runs successfully with arguments that need quoting.
        run(&quiet(), dir.path(), &t, "say", &["hello world".into(), "x".into()], &[]).unwrap();
        assert_eq!(quote_for_shell(TaskShell::Sh, "plain-arg"), "plain-arg");
        assert_eq!(quote_for_shell(TaskShell::Sh, "it's"), r"'it'\''s'");
        assert_eq!(quote_for_shell(TaskShell::Cmd, "a \"b\""), "\"a \"\"b\"\"\"");
        assert_eq!(quote_for_shell(TaskShell::PowerShell, "it's"), "'it''s'");
    }

    #[test]
    fn percent_signs_are_escaped_for_cmd() {
        assert_eq!(quote_for_shell(TaskShell::Cmd, "%COMPUTERNAME%"), "^%COMPUTERNAME^%");
        assert_eq!(quote_for_shell(TaskShell::Cmd, "50% off"), "50^%\" off\"");
        assert_eq!(quote_for_shell(TaskShell::Sh, "%PATH%"), "%PATH%");
    }

    #[cfg(windows)]
    #[test]
    fn cmd_does_not_expand_forwarded_percent_variables() {
        let dir = tempfile::tempdir().unwrap();
        let t = tasks(
            "say = { run = \"echo > out.txt\", shell = \"cmd\" }
",
        );
        let quiet = quiet();
        run(&quiet, dir.path(), &t, "say", &["%COMPUTERNAME%".into(), "a b".into()], &[]).unwrap();
        let written = std::fs::read_to_string(dir.path().join("out.txt")).unwrap();
        assert_eq!(written.trim(), "%COMPUTERNAME% \"a b\"");
    }

    #[test]
    fn powershell_stops_at_a_failing_command_and_keeps_its_exit_code() {
        if process::find_on_path("pwsh").is_none() {
            eprintln!("skipping: no pwsh available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let quiet = quiet();
        let fail = if cfg!(windows) { "cmd /c exit 3" } else { "sh -c 'exit 3'" };
        let t = tasks(&format!(
            "bad = {{ run = [\"{fail}\", \"echo done > out.txt\"], shell = \"pwsh\" }}
"
        ));
        let err = run(&quiet, dir.path(), &t, "bad", &[], &[]).unwrap_err();
        assert!(err.to_string().contains("exit code 3"), "{err}");
        assert!(!dir.path().join("out.txt").exists());

        let one_script = tasks(&format!(
            "bad = {{ run = \"{fail}; echo done > out.txt\", shell = \"pwsh\" }}
"
        ));
        let err = run(&quiet, dir.path(), &one_script, "bad", &[], &[]).unwrap_err();
        assert!(err.to_string().contains("exit code 3"), "{err}");
        assert!(!dir.path().join("out.txt").exists());
    }

    #[test]
    fn a_missing_script_is_an_error_before_anything_runs() {
        let dir = tempfile::tempdir().unwrap();
        let t = tasks(
            "gen = { script = \"scripts/gen.kts\" }
ci = { depends-on = [\"gen\"], run = \"echo done > out.txt\" }
",
        );
        let err = run(&quiet(), dir.path(), &t, "ci", &[], &[]).unwrap_err().to_string();
        assert!(err.contains("task `gen`: script `scripts") && err.contains("not found"), "{err}");
        assert!(!dir.path().join("out.txt").exists(), "the dependent task did not run");
    }

    #[test]
    fn hook_selection() {
        let t = tasks("gen = { run = \"x\", hooks = [\"pre-build\"] }\nother = \"y\"\n");
        let hooked: Vec<_> = tasks_for_hook(&t, Hook::PreBuild).iter().map(|t| t.name.clone()).collect();
        assert_eq!(hooked, ["gen"]);
        assert!(tasks_for_hook(&t, Hook::PostBuild).is_empty());
    }
}
