//! Extensibility:
//!
//! * **Hook plugins** (`[plugins.x] command = "..."`) run at lifecycle hooks
//!   with build information passed through `KARGO_*` environment variables.
//! * **Subcommand plugins**: any `kargo-<name>` executable in `~/.kargo/bin` or
//!   on `PATH` becomes `kargo <name>`, just like Cargo.
//! * **Compiler plugins** (`[plugins.x] maven = "..."`) are handled by the build.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

use crate::tasks;
use crate::workspace::{Project, Workspace};
use kargo_manifest::{Hook, ProcessPlugin};
use kargo_util::config::Config;
use kargo_util::process;
use kargo_util::shell::Shell;

/// Environment shared with tasks and plugins.
pub fn base_env(ws: &Workspace, profile: &str) -> Vec<(String, String)> {
    let m = ws.manifest();
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "kargo".to_owned());
    let mut env = vec![
        ("KARGO".to_owned(), exe),
        ("KARGO_MANIFEST_DIR".to_owned(), ws.root().to_string_lossy().into_owned()),
        ("KARGO_PKG_NAME".to_owned(), m.package.name.clone()),
        ("KARGO_PKG_VERSION".to_owned(), m.package.version.clone()),
        ("KARGO_PKG_GROUP".to_owned(), m.package.group.clone().unwrap_or_default()),
        ("KARGO_TARGET_DIR".to_owned(), ws.target_dir().to_string_lossy().into_owned()),
        ("KARGO_PROFILE".to_owned(), profile.to_owned()),
        ("KARGO_KOTLIN_VERSION".to_owned(), m.kotlin.version.clone()),
    ];
    if let Some(workspace) = ws.workspace() {
        env.push(("KARGO_WORKSPACE_DIR".to_owned(), workspace.root.to_string_lossy().into_owned()));
    }
    env
}

/// Environment of a `[workspace.tasks]` task, which runs at the workspace root.
pub fn workspace_env(project: &Project) -> Vec<(String, String)> {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "kargo".to_owned());
    let root = project.root().to_string_lossy().into_owned();
    let mut env = vec![
        ("KARGO".to_owned(), exe),
        ("KARGO_MANIFEST_DIR".to_owned(), root.clone()),
        ("KARGO_WORKSPACE_DIR".to_owned(), root),
        ("KARGO_TARGET_DIR".to_owned(), project.target_dir().to_string_lossy().into_owned()),
        ("KARGO_PROFILE".to_owned(), "dev".to_owned()),
    ];
    if let Some(&first) = project.members().first() {
        env.push(("KARGO_KOTLIN_VERSION".to_owned(), project.package(first).manifest().kotlin.version.clone()));
    }
    env
}

/// Runs the tasks and hook plugins attached to `hook`.
pub fn run_hook(shell: &Shell, ws: &Workspace, hook: Hook, env: &[(String, String)]) -> Result<()> {
    let manifest = ws.manifest();
    let mut env = env.to_vec();
    env.push(("KARGO_HOOK".to_owned(), hook.as_str().to_owned()));

    for task in tasks::tasks_for_hook(&manifest.tasks, hook) {
        tasks::run(shell, ws.root(), &manifest.tasks, &task.name, &[], &env)?;
    }

    for plugin in manifest.process_plugins.iter().filter(|p| p.hooks.contains(&hook)) {
        shell.status("Plugin", format!("{} ({hook})", plugin.name));
        let mut cmd = plugin_command(ws.root(), plugin)?;
        cmd.args(plugin.platform_args())
            .arg(hook.as_str())
            .current_dir(ws.root())
            .envs(env.iter().map(|(k, v)| (k, v)))
            .envs(plugin.env.iter());
        let status = process::status(shell, &mut cmd, &format!("plugin `{}`", plugin.name))?;
        if !status.success() {
            bail!("plugin `{}` failed at {hook} ({})", plugin.name, process::describe_status(status));
        }
    }
    Ok(())
}

/// Builds the command for a hook plugin. Windows can't start a `.sh` script
/// directly, so there it runs through `sh` (from Git for Windows, for example).
fn plugin_command(root: &Path, plugin: &ProcessPlugin) -> Result<Command> {
    let program = resolve_command(root, plugin.platform_command());
    let is_sh_script = program.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("sh"));
    if !cfg!(windows) || !is_sh_script {
        return Ok(Command::new(program));
    }
    let Some(sh) = process::find_posix_shell("sh") else {
        bail!(
            "plugin `{}`: `{}` is a shell script and Windows has no `sh` to run it; install Git for Windows or set `windows-command`",
            plugin.name,
            plugin.platform_command()
        );
    };
    let mut cmd = Command::new(sh);
    cmd.arg(process::slash(&program));
    Ok(cmd)
}

/// Commands containing a path separator are relative to the project root.
fn resolve_command(root: &Path, command: &str) -> PathBuf {
    if command.contains('/') || command.contains('\\') {
        let p = Path::new(command);
        if p.is_absolute() { p.to_path_buf() } else { root.join(p) }
    } else {
        process::find_on_path(command).unwrap_or_else(|| PathBuf::from(command))
    }
}

/// Finds the executable for `kargo <name>`.
pub fn find_subcommand(config: &Config, name: &str) -> Option<PathBuf> {
    let file = format!("kargo-{name}");
    let in_bin = config.bin_dir().join(if cfg!(windows) { format!("{file}.exe") } else { file.clone() });
    if in_bin.is_file() {
        return Some(in_bin);
    }
    #[cfg(windows)]
    for ext in ["bat", "cmd"] {
        let candidate = config.bin_dir().join(format!("{file}.{ext}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    process::find_on_path(&file)
}

/// Lists `kargo-*` subcommands found in `~/.kargo/bin` and on `PATH`.
pub fn list_subcommands(config: &Config) -> BTreeMap<String, PathBuf> {
    let mut found = BTreeMap::new();
    let mut dirs = vec![config.bin_dir()];
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let Some(name) = stem.strip_prefix("kargo-") else { continue };
            if cfg!(windows) && !path.extension().is_some_and(|e| matches!(e.to_str(), Some("exe" | "bat" | "cmd"))) {
                continue;
            }
            if path.is_file() {
                found.entry(name.to_owned()).or_insert(path);
            }
        }
    }
    found
}

/// Runs an external subcommand, forwarding arguments and the exit code.
pub fn run_subcommand(config: &Config, name: &str, program: &Path, args: &[String], cwd: &Path) -> Result<i32> {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(cwd);
    if let Ok(ws) = Workspace::discover(cwd, None) {
        cmd.envs(base_env(&ws, "dev"));
    }
    let status = process::status(config.shell(), &mut cmd, &format!("`kargo {name}`"))?;
    Ok(process::exit_code_of(status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::Verbosity;

    fn workspace(dir: &Path, extra: &str) -> Workspace {
        std::fs::write(dir.join("kargo.toml"), format!("[package]\nname = \"p\"\nversion = \"1.0\"\n{extra}")).unwrap();
        Workspace::load(&dir.join("kargo.toml")).unwrap()
    }

    #[test]
    fn hooks_run_tasks_and_plugins_with_env() {
        let dir = tempfile::tempdir().unwrap();
        let (task_cmd, plugin) = if cfg!(windows) {
            ("echo %KARGO_HOOK% %KARGO_PKG_NAME%> task.txt", "plugin.bat")
        } else {
            ("echo $KARGO_HOOK $KARGO_PKG_NAME > task.txt", "plugin.sh")
        };
        let plugin_path = dir.path().join(plugin);
        if cfg!(windows) {
            std::fs::write(&plugin_path, "@echo %1 %KARGO_PKG_VERSION% %EXTRA%> plugin.txt\r\n").unwrap();
        } else {
            std::fs::write(&plugin_path, "#!/bin/sh\necho $1 $KARGO_PKG_VERSION $EXTRA > plugin.txt\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&plugin_path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let ws = workspace(
            dir.path(),
            &format!(
                "[tasks]\ngen = {{ run = '{task_cmd}', hooks = [\"pre-build\"] }}\n[plugins.notify]\ncommand = \"./{plugin}\"\nhooks = [\"post-build\"]\nenv = {{ EXTRA = \"yes\" }}\n"
            ),
        );
        let shell = Shell::new(Verbosity::Quiet);
        let env = base_env(&ws, "dev");
        run_hook(&shell, &ws, Hook::PreBuild, &env).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("task.txt")).unwrap().trim(), "pre-build p");
        assert!(!dir.path().join("plugin.txt").exists());
        run_hook(&shell, &ws, Hook::PostBuild, &env).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("plugin.txt")).unwrap().trim(), "post-build 1.0 yes");
    }

    #[test]
    fn failing_plugin_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = if cfg!(windows) { "fail.bat" } else { "fail.sh" };
        let path = dir.path().join(cmd);
        std::fs::write(&path, if cfg!(windows) { "@exit /b 4\r\n" } else { "#!/bin/sh\nexit 4\n" }).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let ws = workspace(dir.path(), &format!("[plugins.bad]\ncommand = \"./{cmd}\"\nhooks = [\"pre-test\"]\n"));
        let err = run_hook(&Shell::new(Verbosity::Quiet), &ws, Hook::PreTest, &[]).unwrap_err();
        assert!(err.to_string().contains("plugin `bad` failed"), "{err}");
    }

    #[test]
    fn windows_command_replaces_command_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let unix = dir.path().join("plugin.sh");
        std::fs::write(
            &unix,
            "#!/bin/sh
echo unix $1 > plugin.txt
",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&unix, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            dir.path().join("plugin.bat"),
            "@echo windows %1> plugin.txt
",
        )
        .unwrap();
        let ws = workspace(
            dir.path(),
            "[plugins.notify]
command = \"./plugin.sh\"
windows-command = \"./plugin.bat\"
hooks = [\"post-build\"]
",
        );
        run_hook(&Shell::new(Verbosity::Quiet), &ws, Hook::PostBuild, &[]).unwrap();
        let expected = if cfg!(windows) { "windows post-build" } else { "unix post-build" };
        assert_eq!(std::fs::read_to_string(dir.path().join("plugin.txt")).unwrap().trim(), expected);
    }

    #[test]
    fn shell_script_plugins_run_on_every_platform() {
        if process::find_posix_shell("sh").is_none() {
            eprintln!("skipping: no sh available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("notify.sh");
        std::fs::write(
            &script,
            "#!/bin/sh
echo \"$1 $KARGO_HOOK\" > plugin.txt
",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let ws = workspace(
            dir.path(),
            "[plugins.notify]
command = \"./notify.sh\"
hooks = [\"post-build\"]
",
        );
        let env = [("KARGO_HOOK".to_owned(), "post-build".to_owned())];
        run_hook(&Shell::new(Verbosity::Quiet), &ws, Hook::PostBuild, &env).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("plugin.txt")).unwrap().trim(), "post-build post-build");
    }

    #[test]
    fn discovers_subcommands_in_bin_dir() {
        let home = tempfile::tempdir().unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        std::fs::create_dir_all(config.bin_dir()).unwrap();
        let name = if cfg!(windows) { "kargo-hello.bat" } else { "kargo-hello" };
        let path = config.bin_dir().join(name);
        std::fs::write(&path, if cfg!(windows) { "@exit /b 7\r\n" } else { "#!/bin/sh\nexit 7\n" }).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let found = find_subcommand(&config, "hello").unwrap();
        assert_eq!(found, path);
        assert!(list_subcommands(&config).contains_key("hello"));
        assert!(find_subcommand(&config, "definitely-missing-subcommand").is_none());
        let code = run_subcommand(&config, "hello", &found, &[], home.path()).unwrap();
        assert_eq!(code, 7);
    }
}
