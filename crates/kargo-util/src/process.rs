//! Helpers for spawning external tools.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};

use crate::shell::Shell;

/// Renders a command for `--verbose` output.
pub fn describe(cmd: &Command) -> String {
    let mut out = cmd.get_program().to_string_lossy().into_owned();
    for arg in cmd.get_args() {
        let arg = arg.to_string_lossy();
        out.push(' ');
        if arg.contains(char::is_whitespace) || arg.is_empty() {
            out.push('"');
            out.push_str(&arg);
            out.push('"');
        } else {
            out.push_str(&arg);
        }
    }
    out
}

/// Runs `cmd` with inherited stdio and fails if it exits unsuccessfully.
pub fn run(shell: &Shell, cmd: &mut Command, what: &str) -> Result<()> {
    let status = status(shell, cmd, what)?;
    if !status.success() {
        bail!("{what} failed ({})", describe_status(status));
    }
    Ok(())
}

/// Runs `cmd` with inherited stdio and returns its exit status.
pub fn status(shell: &Shell, cmd: &mut Command, what: &str) -> Result<ExitStatus> {
    shell.verbose("Running", format!("`{}`", describe(cmd)));
    cmd.status().with_context(|| format!("failed to start {what} (`{}`)", cmd.get_program().to_string_lossy()))
}

pub fn describe_status(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "terminated by signal".to_owned(),
    }
}

/// Quotes an argument for a Kotlin/javac `@argfile`. Backslashes and quotes
/// are escaped, and paths use forward slashes, which both tools accept.
pub fn quote_arg(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Writes an argument file, one quoted argument per line.
pub fn write_argfile(path: &Path, args: &[String]) -> Result<()> {
    let contents: String = args.iter().map(|a| quote_arg(a) + "\n").collect();
    crate::fs::write_string_atomic(path, &contents)
}

/// Converts a path to a string with forward slashes (portable in argfiles).
pub fn slash(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) { s.replace('\\', "/") } else { s.into_owned() }
}

/// Finds a POSIX shell (`sh` or `bash`) to run scripts with. On Windows this
/// skips the WSL launcher and falls back to the shell bundled with Git for
/// Windows, which is usually not on `PATH`.
pub fn find_posix_shell(name: &str) -> Option<PathBuf> {
    if !cfg!(windows) {
        return find_on_path(name);
    }
    let is_wsl_launcher = |p: &Path| {
        let p = p.to_string_lossy().to_lowercase();
        p.contains("\\system32\\") || p.contains("\\windowsapps\\")
    };
    if let Some(found) = find_on_path(name).filter(|p| !is_wsl_launcher(p)) {
        return Some(found);
    }
    // git.exe lives in Git\cmd, Git\bin or Git\mingw64\bin; sh.exe in Git\bin or Git\usr\bin.
    let mut roots: Vec<PathBuf> =
        find_on_path("git").map(|git| git.ancestors().skip(1).take(3).map(Path::to_path_buf).collect()).unwrap_or_default();
    for var in ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
        if let Some(dir) = std::env::var_os(var).map(PathBuf::from) {
            roots.push(dir.join("Git"));
            roots.push(dir.join("Programs").join("Git"));
        }
    }
    roots
        .iter()
        .flat_map(|root| [root.join("bin"), root.join("usr").join("bin")])
        .map(|dir| dir.join(format!("{name}.exe")))
        .find(|p| p.is_file())
}

/// Finds an executable on `PATH`, honoring `PATHEXT` on Windows.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.BAT;.CMD;.COM".to_owned())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_lowercase())
            .chain(std::iter::once(String::new()))
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path)
        .find_map(|dir| exts.iter().map(|ext| dir.join(format!("{name}{ext}"))).find(|candidate| is_executable(candidate)))
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_argfile_entries() {
        assert_eq!(quote_arg("plain"), "\"plain\"");
        assert_eq!(quote_arg(r#"C:\a b\"c""#), r#""C:\\a b\\\"c\"""#);
    }

    #[test]
    fn describes_commands() {
        let mut cmd = Command::new("java");
        cmd.args(["-cp", "a b", ""]);
        assert_eq!(describe(&cmd), r#"java -cp "a b" """#);
    }

    #[test]
    fn argfile_round_trip_format() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("args");
        write_argfile(&p, &["-d".into(), "out dir".into()]).unwrap();
        assert_eq!(std::fs::read_to_string(p).unwrap(), "\"-d\"\n\"out dir\"\n");
    }

    #[test]
    fn missing_executable_is_none() {
        assert!(find_on_path("definitely-not-a-real-kargo-binary").is_none());
    }
}
