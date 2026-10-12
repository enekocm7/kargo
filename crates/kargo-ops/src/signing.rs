//! Signing published files with GnuPG: a detached, ASCII-armored signature (`<file>.asc`) next
//! to every file, which Maven Central requires.
//!
//! On CI the key usually comes from the environment: `KARGO_SIGNING_KEY` holds an exported
//! secret key (`gpg --export-secret-keys --armor <id>`, as is or base64-encoded), which is
//! imported into a throwaway GnuPG home. Without it, the user's own keyring and agent are used.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use base64::Engine as _;

use kargo_util::process;
use kargo_util::shell::Shell;

/// An exported secret key (ASCII-armored, or that text base64-encoded).
pub const KEY_ENV: &str = "KARGO_SIGNING_KEY";
/// The key to sign with (`gpg --local-user`); the default key otherwise.
pub const KEY_ID_ENV: &str = "KARGO_SIGNING_KEY_ID";
/// The key's passphrase, if it has one.
pub const PASSPHRASE_ENV: &str = "KARGO_SIGNING_KEY_PASSPHRASE";
/// The `gpg` executable; `gpg` on `PATH` otherwise.
pub const GPG_ENV: &str = "KARGO_GPG";

/// Signs files with one key.
pub struct Signer {
    gpg: PathBuf,
    /// A temporary GnuPG home holding the key from `KARGO_SIGNING_KEY`, removed on drop.
    home: Option<PathBuf>,
    key_id: Option<String>,
    passphrase: Option<String>,
    shell: Shell,
}

impl Signer {
    /// A signer configured from `KARGO_SIGNING_KEY`, `KARGO_SIGNING_KEY_ID`, `KARGO_SIGNING_KEY_PASSPHRASE` and `KARGO_GPG`.
    pub fn from_env(shell: &Shell) -> Result<Self> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let gpg = match var(GPG_ENV) {
            Some(path) => PathBuf::from(path),
            None => process::find_on_path("gpg").context(
                "signing needs GnuPG, but `gpg` is not on PATH; install GnuPG (https://gnupg.org/download/), \
                 set KARGO_GPG to its path, or set `sign = false` in [publish]",
            )?,
        };
        Self::new(shell, gpg, var(KEY_ENV).as_deref(), var(KEY_ID_ENV), var(PASSPHRASE_ENV))
    }

    /// A signer using `gpg`; with `key`, that secret key is imported into a temporary GnuPG home.
    pub fn new(shell: &Shell, gpg: PathBuf, key: Option<&str>, key_id: Option<String>, passphrase: Option<String>) -> Result<Self> {
        let mut signer = Self { gpg, home: None, key_id, passphrase, shell: shell.clone() };
        if let Some(key) = key {
            let key = armored_key(key)?;
            // Kept short: gpg-agent's socket path must fit in a Unix socket address.
            let home = std::env::temp_dir().join(format!("kargo-gpg-{}", std::process::id()));
            std::fs::create_dir_all(&home).with_context(|| format!("failed to create {}", home.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700));
            }
            signer.home = Some(home);
            let mut cmd = signer.command();
            cmd.args(["--batch", "--import"]);
            signer.run(cmd, key.as_bytes(), "import the signing key from KARGO_SIGNING_KEY")?;
        }
        Ok(signer)
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.gpg);
        if let Some(home) = &self.home {
            cmd.arg("--homedir").arg(homedir_arg(&self.gpg, home));
        }
        cmd
    }

    /// Writes `<file>.asc`, a detached ASCII-armored signature of `file`, and returns its path.
    pub fn sign(&self, file: &Path) -> Result<PathBuf> {
        let mut asc = file.as_os_str().to_owned();
        asc.push(".asc");
        let asc = PathBuf::from(asc);
        let mut cmd = self.command();
        cmd.args(["--yes", "--armor", "--detach-sign"]);
        if let Some(id) = &self.key_id {
            cmd.args(["--local-user", id]);
        }
        let input: &[u8] = match &self.passphrase {
            Some(passphrase) => {
                cmd.args(["--batch", "--pinentry-mode", "loopback", "--passphrase-fd", "0"]);
                passphrase.as_bytes()
            }
            // A key without a passphrase, or one gpg-agent asks for itself.
            None if self.home.is_some() => {
                cmd.arg("--batch");
                b""
            }
            None => b"",
        };
        cmd.arg("--output").arg(&asc).arg(file);
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.run(cmd, input, &format!("sign {name}"))?;
        Ok(asc)
    }

    fn run(&self, mut cmd: Command, input: &[u8], what: &str) -> Result<()> {
        self.shell.verbose("Running", format!("`{}`", process::describe(&cmd)));
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to start `{}`", self.gpg.display()))?;
        if let Some(mut stdin) = child.stdin.take() {
            // gpg may exit before reading everything (e.g. on a bad key); that shows in its status.
            let _ = stdin.write_all(input);
        }
        let output = child.wait_with_output().with_context(|| format!("failed to wait for `{}`", self.gpg.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let hint = if self.passphrase.is_none() && stderr.contains("passphrase") {
                format!("; set {PASSPHRASE_ENV} if the key has a passphrase")
            } else {
                String::new()
            };
            bail!("could not {what} with GnuPG ({}){hint}:\n{}", process::describe_status(output.status), stderr.trim());
        }
        Ok(())
    }
}

impl Drop for Signer {
    fn drop(&mut self) {
        if let Some(home) = self.home.take() {
            // Stop the agent the imported key started, then remove the key.
            let gpgconf = self.gpg.with_file_name(if cfg!(windows) { "gpgconf.exe" } else { "gpgconf" });
            let _ = Command::new(gpgconf)
                .arg("--homedir")
                .arg(homedir_arg(&self.gpg, &home))
                .args(["--kill", "gpg-agent"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = std::fs::remove_dir_all(&home);
        }
    }
}

/// `--homedir` for `gpg`. The GnuPG of Git for Windows and MSYS2 (`...\usr\bin\gpg.exe`) cannot
/// start its agent in a home spelled `C:\...` (a socket name may not contain `:`), so it gets `/c/...`.
fn homedir_arg(gpg: &Path, home: &Path) -> std::ffi::OsString {
    let msys = gpg.parent().is_some_and(|bin| bin.ends_with(Path::new("usr").join("bin")));
    if !(cfg!(windows) && msys) {
        return home.as_os_str().to_owned();
    }
    let text = home.to_string_lossy().replace('\\', "/");
    match text.split_once(":/") {
        Some((drive, rest)) if drive.len() == 1 => format!("/{}/{rest}", drive.to_ascii_lowercase()).into(),
        _ => text.into(),
    }
}

/// The ASCII-armored key from `KARGO_SIGNING_KEY`, decoding it first when it is base64.
fn armored_key(value: &str) -> Result<String> {
    let value = value.trim();
    if value.contains("-----BEGIN PGP PRIVATE KEY BLOCK-----") {
        return Ok(value.replace("\\n", "\n"));
    }
    let compact: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(compact)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|text| text.contains("-----BEGIN PGP PRIVATE KEY BLOCK-----"));
    decoded.with_context(|| {
        format!("{KEY_ENV} must hold an ASCII-armored secret key (`gpg --export-secret-keys --armor <KEY_ID>`), as is or base64-encoded")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::Verbosity;

    const KEY: &str = "-----BEGIN PGP PRIVATE KEY BLOCK-----\n\nxyz\n-----END PGP PRIVATE KEY BLOCK-----";

    #[test]
    fn msys_gnupg_gets_posix_home_paths() {
        let home = Path::new(r"C:\Users\me\Temp\kargo-gpg-1");
        let msys = homedir_arg(Path::new(r"C:\Program Files\Git\usr\bin\gpg.exe"), home);
        let native = homedir_arg(Path::new(r"C:\Program Files\GnuPG\bin\gpg.exe"), home);
        assert_eq!(native, home.as_os_str());
        if cfg!(windows) {
            assert_eq!(msys, "/c/Users/me/Temp/kargo-gpg-1");
        } else {
            assert_eq!(msys, home.as_os_str());
        }
    }

    #[test]
    fn reads_armored_and_base64_keys() {
        assert_eq!(armored_key(KEY).unwrap(), KEY);
        // CI secrets sometimes keep newlines escaped.
        assert_eq!(armored_key(&KEY.replace('\n', "\\n")).unwrap(), KEY);
        let encoded = base64::engine::general_purpose::STANDARD.encode(KEY);
        assert_eq!(armored_key(&encoded).unwrap(), KEY);
        assert!(armored_key("not a key").unwrap_err().to_string().contains(KEY_ENV));
    }

    /// Signs a file with a key generated on the spot, when GnuPG is installed.
    #[test]
    fn signs_with_an_imported_key() {
        let Some(gpg) = process::find_on_path("gpg") else { return };
        let shell = Shell::new(Verbosity::Quiet);
        let dir = tempfile::tempdir().unwrap();
        let keyring = std::env::temp_dir().join(format!("kargo-gpgtest-{}", std::process::id()));
        std::fs::create_dir_all(&keyring).unwrap();
        let gpg_in = |args: &[&str]| Command::new(&gpg).arg("--homedir").arg(homedir_arg(&gpg, &keyring)).args(args).output().unwrap();
        let generated =
            gpg_in(&["--batch", "--passphrase", "", "--quick-gen-key", "Kargo Test <test@example.com>", "ed25519", "sign", "never"]);
        if !generated.status.success() {
            eprintln!("skipping: GnuPG could not generate a key: {}", String::from_utf8_lossy(&generated.stderr));
            // An old or restricted GnuPG; signing itself is covered by the e2e tests.
            let _ = std::fs::remove_dir_all(&keyring);
            return;
        }
        let exported = gpg_in(&["--batch", "--armor", "--export-secret-keys"]);
        let _ = Command::new(gpg.with_file_name(if cfg!(windows) { "gpgconf.exe" } else { "gpgconf" }))
            .arg("--homedir")
            .arg(homedir_arg(&gpg, &keyring))
            .args(["--kill", "gpg-agent"])
            .status();
        let _ = std::fs::remove_dir_all(&keyring);
        let key = String::from_utf8(exported.stdout).unwrap();

        let file = dir.path().join("lib-1.0.jar");
        std::fs::write(&file, "jar").unwrap();
        let signer = Signer::new(&shell, gpg, Some(&key), None, None).unwrap();
        let asc = signer.sign(&file).unwrap();
        assert_eq!(asc, dir.path().join("lib-1.0.jar.asc"));
        assert!(std::fs::read_to_string(&asc).unwrap().starts_with("-----BEGIN PGP SIGNATURE-----"));
        let mut verify = signer.command();
        verify.arg("--verify").arg(&asc).arg(&file);
        assert!(verify.output().unwrap().status.success());
        let home = signer.home.clone().unwrap();
        drop(signer);
        assert!(!home.exists(), "the imported key is removed");
    }
}
