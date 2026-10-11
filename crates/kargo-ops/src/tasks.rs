//! User-defined tasks from `[tasks]`: shell commands with dependencies that
//! can also attach to build lifecycle hooks.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use kargo_manifest::{Hook, Task, TaskShell};
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
            cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
            cmd
        }
    })
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

/// Runs a task (and its dependencies). `args` are appended to the last command
/// of the requested task.
pub fn run(
    shell: &Shell,
    root: &Path,
    tasks: &BTreeMap<String, Task>,
    name: &str,
    args: &[String],
    env: &[(String, String)],
) -> Result<()> {
    let order = plan(tasks, name)?;
    for task in order {
        shell.status("Running", format!("task `{}`", task.name));
        let cwd = task.cwd.as_ref().map(|c| root.join(c)).unwrap_or_else(|| root.to_path_buf());
        let mut task_env: Vec<(String, String)> = env.to_vec();
        task_env.push(("KARGO_TASK".to_owned(), task.name.clone()));
        task_env.extend(task.env.iter().map(|(k, v)| (k.clone(), v.clone())));
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
    if !arg.is_empty() && arg.chars().all(safe) {
        return arg.to_owned();
    }
    match shell {
        TaskShell::Cmd => format!("\"{}\"", arg.replace('"', "\"\"")),
        TaskShell::PowerShell | TaskShell::Pwsh => format!("'{}'", arg.replace('\'', "''")),
        TaskShell::Sh | TaskShell::Bash => format!("'{}'", arg.replace('\'', "'\\''")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_manifest::Manifest;

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
        run(&Shell::new(kargo_util::shell::Verbosity::Quiet), dir.path(), &t, "hello", &[], &[]).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap().trim(), "hi hello");

        let failing = tasks("bad = \"exit 3\"\n");
        let err = run(&Shell::new(kargo_util::shell::Verbosity::Quiet), dir.path(), &failing, "bad", &[], &[]).unwrap_err();
        assert!(err.to_string().contains("exit code 3"), "{err}");
    }

    #[test]
    fn uses_windows_run_on_windows_and_run_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let t = tasks(
            "pick = { run = \"echo unix> out.txt\", windows-run = \"echo windows> out.txt\" }
",
        );
        run(&Shell::new(kargo_util::shell::Verbosity::Quiet), dir.path(), &t, "pick", &[], &[]).unwrap();
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
        run(&Shell::new(kargo_util::shell::Verbosity::Quiet), dir.path(), &t, "posix", &["it's".into()], &[]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("out.txt")).unwrap().trim(), "hi posix it's");
    }

    #[test]
    fn appends_arguments_to_last_command() {
        let dir = tempfile::tempdir().unwrap();
        let t = tasks("say = \"echo\"\n");
        // Only checks that it runs successfully with arguments that need quoting.
        run(&Shell::new(kargo_util::shell::Verbosity::Quiet), dir.path(), &t, "say", &["hello world".into(), "x".into()], &[]).unwrap();
        assert_eq!(quote_for_shell(TaskShell::Sh, "plain-arg"), "plain-arg");
        assert_eq!(quote_for_shell(TaskShell::Sh, "it's"), r"'it'\''s'");
        assert_eq!(quote_for_shell(TaskShell::Cmd, "a \"b\""), "\"a \"\"b\"\"\"");
        assert_eq!(quote_for_shell(TaskShell::PowerShell, "it's"), "'it''s'");
    }

    #[test]
    fn hook_selection() {
        let t = tasks("gen = { run = \"x\", hooks = [\"pre-build\"] }\nother = \"y\"\n");
        let hooked: Vec<_> = tasks_for_hook(&t, Hook::PreBuild).iter().map(|t| t.name.clone()).collect();
        assert_eq!(hooked, ["gen"]);
        assert!(tasks_for_hook(&t, Hook::PostBuild).is_empty());
    }
}
