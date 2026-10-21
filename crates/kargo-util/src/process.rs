//! Helpers for spawning external tools.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};

use anyhow::{Context, Result, bail};

use crate::shell::Shell;

/// Renders a command for `--verbose` output, quoting the arguments so it can be copied into a shell.
pub fn describe(cmd: &Command) -> String {
    let mut out = cmd.get_program().to_string_lossy().into_owned();
    for arg in cmd.get_args() {
        out.push(' ');
        out.push_str(&quote_for_display(&arg.to_string_lossy()));
    }
    out
}

/// Quotes `arg` for a displayed command line when it is empty or has whitespace, quotes or shell
/// metacharacters, so `buy milk` as one argument differs from two. Other arguments are left alone.
pub fn quote_for_display(arg: &str) -> Cow<'_, str> {
    let special = |c: char| c.is_whitespace() || "\"'$`&|;<>()*?!#".contains(c);
    if !arg.is_empty() && !arg.contains(special) {
        return Cow::Borrowed(arg);
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        // Backslashes are path separators on Windows, so only escape them elsewhere.
        if c == '"' || (!cfg!(windows) && matches!(c, '\\' | '$' | '`')) {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    Cow::Owned(out)
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

/// Spawns a background process that outlives Kargo: it gets its own process
/// group (so the terminal's Ctrl-C doesn't reach it) and no console window.
///
/// The caller must redirect all three of `cmd`'s stdio streams. On Windows,
/// `CreateProcess` also hands the child every inheritable handle Kargo holds,
/// including Kargo's own stdin/stdout/stderr, so a long-lived child would keep a
/// pipe Kargo writes to open and whoever reads it (`kargo build | tee`, CI log
/// capture, test harnesses) would never see EOF. Kargo's std handles are made
/// non-inheritable for the duration of the spawn. Unix needs nothing: the
/// standard library opens every descriptor close-on-exec.
pub fn spawn_detached(cmd: &mut Command) -> std::io::Result<Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        cmd.spawn()
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        windows::spawn_without_std_handles(cmd)
    }
    #[cfg(not(any(unix, windows)))]
    {
        cmd.spawn()
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::io::{AsRawHandle, RawHandle};
    use std::process::{Child, Command};
    use std::sync::Mutex;

    const HANDLE_FLAG_INHERIT: u32 = 0x1;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetHandleInformation(handle: RawHandle, flags: *mut u32) -> i32;
        fn SetHandleInformation(handle: RawHandle, mask: u32, flags: u32) -> i32;
    }

    /// Serializes detached spawns so one never restores a flag another just cleared.
    static LOCK: Mutex<()> = Mutex::new(());

    pub(super) fn spawn_without_std_handles(cmd: &mut Command) -> std::io::Result<Child> {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let handles = [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()];
        // The handles to restore afterwards; one shared by two streams is listed once.
        let mut cleared: Vec<RawHandle> = Vec::new();
        for h in handles {
            if h.is_null() || cleared.contains(&h) {
                continue;
            }
            let mut flags = 0;
            // SAFETY: `h` is one of this process's std handles; both calls only
            // read or change its inheritance flag and fail harmlessly if it is invalid.
            let inheritable = unsafe { GetHandleInformation(h, &mut flags) } != 0 && flags & HANDLE_FLAG_INHERIT != 0;
            if inheritable && unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) } != 0 {
                cleared.push(h);
            }
        }
        let result = cmd.spawn();
        for h in cleared {
            // SAFETY: as above.
            unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
        }
        result
    }
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

/// Command lines at or above this length (in characters) pass their classpath through an argfile.
/// Windows rejects command lines over 32,767 characters, and `cmd` over 8,191.
pub const LONG_CLASSPATH: usize = 8000;

/// Joins classpath entries with forward slashes and the platform separator, which the JVM and the
/// JUnit launcher read the same inside an argfile.
pub fn classpath_for_argfile(paths: &[PathBuf]) -> String {
    let sep = if cfg!(windows) { ";" } else { ":" };
    paths.iter().map(|p| slash(p)).collect::<Vec<_>>().join(sep)
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
    fn classpath_in_an_argfile_uses_forward_slashes() {
        let sep = if cfg!(windows) { ';' } else { ':' };
        let joined = classpath_for_argfile(&[PathBuf::from("a b/x.jar"), PathBuf::from("lib/y.jar")]);
        assert_eq!(joined, format!("a b/x.jar{sep}lib/y.jar"));
    }

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
    fn quotes_displayed_arguments() {
        assert_eq!(quote_for_display("milk"), "milk");
        assert_eq!(quote_for_display("--flag=1"), "--flag=1");
        assert_eq!(quote_for_display("buy milk"), r#""buy milk""#);
        assert_eq!(quote_for_display(""), r#""""#);
        assert_eq!(quote_for_display(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_for_display("a;b"), r#""a;b""#);
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

    /// A detached child must not keep the spawner's stdout pipe open (#82). The
    /// test re-runs itself: the outer run reads a "parent" run's stdout through a
    /// pipe, the parent detaches a long-lived "grandchild" and exits, and the pipe
    /// must reach EOF while the grandchild is still running.
    #[test]
    fn detached_child_does_not_hold_parent_stdio() {
        const ROLE: &str = "KARGO_TEST_DETACH_ROLE";
        const STOP: &str = "KARGO_TEST_DETACH_STOP";
        let exe = std::env::current_exe().unwrap();
        let self_args = ["--exact", "process::tests::detached_child_does_not_hold_parent_stdio", "--test-threads=1"];
        match std::env::var(ROLE).as_deref() {
            Ok("grandchild") => {
                let stop = PathBuf::from(std::env::var_os(STOP).unwrap());
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while !stop.exists() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            Ok("parent") => {
                let mut cmd = Command::new(&exe);
                cmd.args(self_args)
                    .env(ROLE, "grandchild")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                // Deliberately left running: it must outlive this process.
                #[allow(clippy::zombie_processes)]
                let _grandchild = spawn_detached(&mut cmd).unwrap();
            }
            _ => {
                let dir = tempfile::tempdir().unwrap();
                let stop = dir.path().join("stop");
                let mut parent = Command::new(&exe)
                    .args(self_args)
                    .env(ROLE, "parent")
                    .env(STOP, &stop)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                let mut out = parent.stdout.take().unwrap();
                let mut err = parent.stderr.take().unwrap();
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let mut sink = Vec::new();
                    let _ = std::io::Read::read_to_end(&mut out, &mut sink);
                    let _ = std::io::Read::read_to_end(&mut err, &mut sink);
                    let _ = tx.send(());
                });
                let eof = rx.recv_timeout(std::time::Duration::from_secs(30));
                std::fs::write(&stop, "").unwrap();
                assert!(parent.wait().unwrap().success());
                assert!(eof.is_ok(), "the detached grandchild kept the parent's stdout/stderr pipe open");
            }
        }
    }
}
