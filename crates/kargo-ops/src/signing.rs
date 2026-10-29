//! Signing published files: a detached, ASCII-armored OpenPGP signature (`<file>.asc`) next to
//! every file, which Maven Central requires.
//!
//! On CI the key usually comes from the environment: `KARGO_SIGNING_KEY` holds an exported
//! secret key (`gpg --export-secret-keys --armor <id>`, as is or base64-encoded), and Kargo signs
//! with it in memory, without GnuPG (like Gradle's in-memory PGP keys). With `KARGO_GPG` set as
//! well, the key is imported into a throwaway GnuPG home instead. Without a key, GnuPG signs with
//! the user's own keyring and agent.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use pgp::composed::{ArmorOptions, Deserializable, DetachedSignature, SignedSecretKey};
use pgp::crypto::hash::HashAlgorithm;
use pgp::types::{KeyDetails, Password};

use kargo_util::process;
use kargo_util::shell::Shell;

/// An exported secret key (ASCII-armored, or that text base64-encoded).
pub const KEY_ENV: &str = "KARGO_SIGNING_KEY";
/// The key to sign with (`gpg --local-user`); the default key otherwise.
pub const KEY_ID_ENV: &str = "KARGO_SIGNING_KEY_ID";
/// The key's passphrase, if it has one.
pub const PASSPHRASE_ENV: &str = "KARGO_SIGNING_KEY_PASSPHRASE";
/// Another name for [`PASSPHRASE_ENV`], the one Gradle users know.
pub const PASSWORD_ENV: &str = "KARGO_SIGNING_PASSWORD";
/// The `gpg` executable; `gpg` on `PATH` otherwise. With `KARGO_SIGNING_KEY`, it makes GnuPG sign
/// instead of Kargo itself.
pub const GPG_ENV: &str = "KARGO_GPG";

/// Signs files with one key.
pub struct Signer {
    backend: Backend,
}

enum Backend {
    GnuPg(GnuPg),
    InMemory(Box<InMemoryKey>),
}

impl Signer {
    /// A signer configured from `KARGO_SIGNING_KEY`, `KARGO_SIGNING_KEY_ID`, `KARGO_SIGNING_KEY_PASSPHRASE`
    /// (or `KARGO_SIGNING_PASSWORD`) and `KARGO_GPG`.
    pub fn from_env(shell: &Shell) -> Result<Self> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let key = var(KEY_ENV);
        let passphrase = var(PASSPHRASE_ENV).or_else(|| var(PASSWORD_ENV));
        if let (Some(key), None) = (&key, var(GPG_ENV)) {
            return Self::in_memory(key, var(KEY_ID_ENV).as_deref(), passphrase);
        }
        let gpg = match var(GPG_ENV) {
            Some(path) => PathBuf::from(path),
            None => process::find_on_path("gpg").context(
                "signing needs a key: export KARGO_SIGNING_KEY (`gpg --export-secret-keys --armor <KEY_ID>`) or install GnuPG \
                 (https://gnupg.org/download/) to sign with your keyring, or set `sign = false` in [publish]",
            )?,
        };
        Self::new(shell, gpg, key.as_deref(), var(KEY_ID_ENV), passphrase)
    }

    /// A signer that signs in memory with an exported secret key, without GnuPG.
    pub fn in_memory(key: &str, key_id: Option<&str>, passphrase: Option<String>) -> Result<Self> {
        Ok(Self { backend: Backend::InMemory(Box::new(InMemoryKey::new(key, key_id, passphrase)?)) })
    }

    /// A signer using `gpg`; with `key`, that secret key is imported into a temporary GnuPG home.
    pub fn new(shell: &Shell, gpg: PathBuf, key: Option<&str>, key_id: Option<String>, passphrase: Option<String>) -> Result<Self> {
        Ok(Self { backend: Backend::GnuPg(GnuPg::new(shell, gpg, key, key_id, passphrase)?) })
    }

    /// Writes `<file>.asc`, a detached ASCII-armored signature of `file`, and returns its path.
    pub fn sign(&self, file: &Path) -> Result<PathBuf> {
        let mut asc = file.as_os_str().to_owned();
        asc.push(".asc");
        let asc = PathBuf::from(asc);
        match &self.backend {
            Backend::GnuPg(gpg) => gpg.sign(file, &asc)?,
            Backend::InMemory(key) => key.sign(file, &asc)?,
        }
        Ok(asc)
    }
}

/// A secret key read from `KARGO_SIGNING_KEY`, which signs without GnuPG.
struct InMemoryKey {
    key: SignedSecretKey,
    /// Which key of `key` signs: `None` for the primary key, else the index of a secret subkey.
    subkey: Option<usize>,
    passphrase: Password,
}

impl InMemoryKey {
    fn new(text: &str, key_id: Option<&str>, passphrase: Option<String>) -> Result<Self> {
        let armored = armored_key(text)?;
        let (key, _) = SignedSecretKey::from_string(&armored)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("could not read the secret key in {KEY_ENV}; set {GPG_ENV} to sign with GnuPG instead"))?;
        let subkey = signing_key(&key, key_id)?;
        let encrypted = match subkey {
            None => key.primary_key.secret_params().is_encrypted(),
            Some(i) => key.secret_subkeys[i].key.secret_params().is_encrypted(),
        };
        if encrypted && passphrase.is_none() {
            bail!("the key in {KEY_ENV} is protected by a passphrase; export it as {PASSPHRASE_ENV}");
        }
        let passphrase = passphrase.map(Password::from).unwrap_or_else(Password::empty);
        Ok(Self { key, subkey, passphrase })
    }

    fn sign(&self, file: &Path, asc: &Path) -> Result<()> {
        let data = std::fs::File::open(file).with_context(|| format!("failed to read {}", file.display()))?;
        let data = std::io::BufReader::new(data);
        let rng = rand::rngs::OsRng;
        let signature = match self.subkey {
            None => DetachedSignature::sign_binary_data(rng, &self.key.primary_key, &self.passphrase, HashAlgorithm::Sha512, data),
            Some(i) => {
                DetachedSignature::sign_binary_data(rng, &self.key.secret_subkeys[i].key, &self.passphrase, HashAlgorithm::Sha512, data)
            }
        };
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let signature = signature.map_err(|e| anyhow::anyhow!("{e}")).with_context(|| {
            format!("could not sign {name} with the key in {KEY_ENV}; check {PASSPHRASE_ENV} if the key has a passphrase")
        })?;
        let text = signature.to_armored_string(ArmorOptions::default()).map_err(|e| anyhow::anyhow!("{e}"))?;
        std::fs::write(asc, text).with_context(|| format!("failed to write {}", asc.display()))
    }
}

/// The key of `key` that signs, like GnuPG picks it: the one `key_id` names (a fingerprint or key
/// ID, matched at its end), else the newest subkey that can sign, else the primary key.
fn signing_key(key: &SignedSecretKey, key_id: Option<&str>) -> Result<Option<usize>> {
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02X}")).collect::<String>();
    let ids = |fingerprint: String, key_id: String| [fingerprint, key_id];
    if let Some(wanted) = key_id {
        let wanted: String =
            wanted.trim().trim_start_matches("0x").chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase();
        let matches = |names: [String; 2]| !wanted.is_empty() && names.iter().any(|n| n.ends_with(&wanted));
        if matches(ids(format!("{:X}", key.primary_key.fingerprint()), hex(key.primary_key.legacy_key_id().as_ref()))) {
            return Ok(None);
        }
        if let Some(i) =
            key.secret_subkeys.iter().position(|s| matches(ids(format!("{:X}", s.key.fingerprint()), hex(s.key.legacy_key_id().as_ref()))))
        {
            return Ok(Some(i));
        }
        bail!("the key in {KEY_ENV} has no key `{wanted}` ({KEY_ID_ENV}); list its keys with `gpg --list-secret-keys --keyid-format long`");
    }
    let signing = key.secret_subkeys.iter().rposition(|s| s.signatures.iter().any(|sig| sig.key_flags().sign()));
    Ok(signing)
}

/// Signs with the `gpg` executable.
struct GnuPg {
    gpg: PathBuf,
    /// A temporary GnuPG home holding the key from `KARGO_SIGNING_KEY`, removed on drop.
    home: Option<PathBuf>,
    key_id: Option<String>,
    passphrase: Option<String>,
    shell: Shell,
}

impl GnuPg {
    fn new(shell: &Shell, gpg: PathBuf, key: Option<&str>, key_id: Option<String>, passphrase: Option<String>) -> Result<Self> {
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

    fn sign(&self, file: &Path, asc: &Path) -> Result<()> {
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
        cmd.arg("--output").arg(asc).arg(file);
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.run(cmd, input, &format!("sign {name}"))
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

impl Drop for GnuPg {
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
        let signer = Signer::new(&shell, gpg.clone(), Some(&key), None, None).unwrap();
        let asc = signer.sign(&file).unwrap();
        assert_eq!(asc, dir.path().join("lib-1.0.jar.asc"));
        assert!(std::fs::read_to_string(&asc).unwrap().starts_with("-----BEGIN PGP SIGNATURE-----"));
        let Backend::GnuPg(gnupg) = &signer.backend else { panic!("signs with GnuPG") };
        let mut verify = gnupg.command();
        verify.arg("--verify").arg(&asc).arg(&file);
        assert!(verify.output().unwrap().status.success());
        let home = gnupg.home.clone().unwrap();
        drop(signer);
        assert!(!home.exists(), "the imported key is removed");

        // The same exported key signs in memory, and GnuPG accepts the signature.
        let file = dir.path().join("lib-1.0.pom");
        std::fs::write(&file, "<project/>").unwrap();
        let asc = Signer::in_memory(&key, None, None).unwrap().sign(&file).unwrap();
        let checker = Signer::new(&shell, gpg, Some(&key), None, None).unwrap();
        let Backend::GnuPg(gnupg) = &checker.backend else { unreachable!() };
        let verified = gnupg.command().arg("--verify").arg(&asc).arg(&file).output().unwrap();
        assert!(verified.status.success(), "{}", String::from_utf8_lossy(&verified.stderr));
    }

    /// A key with a primary key that only certifies and a signing subkey, like `gpg --full-generate-key` makes.
    fn generated_key(passphrase: Option<&str>) -> SignedSecretKey {
        use pgp::composed::{EncryptionCaps, KeyType, SecretKeyParamsBuilder, SubkeyParamsBuilder};
        let mut subkey = SubkeyParamsBuilder::default();
        subkey.key_type(KeyType::Ed25519Legacy).can_sign(true).can_encrypt(EncryptionCaps::None).can_authenticate(false);
        if let Some(passphrase) = passphrase {
            subkey.passphrase(Some(passphrase.to_owned()));
        }
        let mut params = SecretKeyParamsBuilder::default();
        params
            .key_type(KeyType::Ed25519Legacy)
            .can_certify(true)
            .can_sign(false)
            .can_encrypt(EncryptionCaps::None)
            .primary_user_id("Kargo Test <test@example.com>".into())
            .subkeys(vec![subkey.build().unwrap()]);
        if let Some(passphrase) = passphrase {
            params.passphrase(Some(passphrase.to_owned()));
        }
        params.build().unwrap().generate(rand::rngs::OsRng).unwrap()
    }

    #[test]
    fn signs_in_memory_with_the_signing_subkey() {
        let key = generated_key(None);
        let armored = key.to_armored_string(ArmorOptions::default()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib-1.0.jar");
        std::fs::write(&file, "jar").unwrap();
        // Base64-encoded, as CI secrets often are.
        let encoded = base64::engine::general_purpose::STANDARD.encode(&armored);
        let asc = Signer::in_memory(&encoded, None, None).unwrap().sign(&file).unwrap();
        let text = std::fs::read_to_string(&asc).unwrap();
        assert!(text.starts_with("-----BEGIN PGP SIGNATURE-----"), "{text}");
        let (signature, _) = DetachedSignature::from_string(&text).unwrap();
        let public = key.secret_subkeys[0].key.public_key();
        signature.verify(public, b"jar").unwrap();
        assert!(signature.verify(public, b"other").is_err());

        // KARGO_SIGNING_KEY_ID picks a key by the end of its fingerprint or key ID.
        let primary = format!("{:X}", key.primary_key.fingerprint());
        let signer = Signer::in_memory(&armored, Some(&primary[primary.len() - 16..].to_lowercase()), None).unwrap();
        let Backend::InMemory(chosen) = &signer.backend else { unreachable!() };
        assert_eq!(chosen.subkey, None);
        let err = Signer::in_memory(&armored, Some("DEADBEEF"), None).err().unwrap().to_string();
        assert!(err.contains("no key `DEADBEEF`"), "{err}");
    }

    #[test]
    fn in_memory_keys_with_a_passphrase_need_it() {
        let key = generated_key(Some("s3cret"));
        let armored = key.to_armored_string(ArmorOptions::default()).unwrap();
        let err = Signer::in_memory(&armored, None, None).err().unwrap().to_string();
        assert!(err.contains(PASSPHRASE_ENV), "{err}");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib-1.0.jar");
        std::fs::write(&file, "jar").unwrap();
        let wrong = Signer::in_memory(&armored, None, Some("nope".into())).unwrap().sign(&file);
        assert!(format!("{:#}", wrong.unwrap_err()).contains("could not sign lib-1.0.jar"));
        let asc = Signer::in_memory(&armored, None, Some("s3cret".into())).unwrap().sign(&file).unwrap();
        let (signature, _) = DetachedSignature::from_string(&std::fs::read_to_string(asc).unwrap()).unwrap();
        signature.verify(key.secret_subkeys[0].key.public_key(), b"jar").unwrap();
    }
}
