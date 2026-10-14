//! Kotlin language servers, which `kargo lsp` runs behind its own server, and the Kotlin debug
//! adapter `kargo dap` runs behind its own.
//!
//! Each is pinned to one version like the compiler:
//!
//! - [kmp-lsp](https://github.com/Hessesian/kmp-lsp) (`kmp-lsp`, the default), a native
//!   tree-sitter based server that needs no JVM, downloaded from GitHub together with its jar
//!   indexer and checked against the `sha256sums.txt` of the release.
//! - [JetBrains' Kotlin LSP](https://github.com/Kotlin/kotlin-lsp) (`kotlin-lsp`),
//!   downloaded from `download.jetbrains.com` and checked against the SHA-256 published next
//!   to it. It bundles its own Java runtime.
//! - [fwcd's kotlin-language-server](https://github.com/fwcd/kotlin-language-server) (`kls`),
//!   downloaded from GitHub and checked against [`KLS_SHA256`]. It runs on the project's JDK,
//!   or on a managed JDK [`KLS_MAX_JDK`] when that is newer.
//! - [fwcd's kotlin-debug-adapter](https://github.com/fwcd/kotlin-debug-adapter) (`kotlin-debug-adapter`),
//!   downloaded from GitHub and checked against [`KDA_SHA256`]. It runs on the project's JDK.
//!
//! All are installed into `~/.kargo/toolchains/<server>/<version>`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use kargo_maven::Version;
use kargo_util::config::Config;
use kargo_util::flock::FileLock;

use crate::archive::{self, ArchiveKind};
use crate::install_lock;

/// The kmp-lsp version `kargo lsp` uses.
pub const KMP_LSP_VERSION: &str = "0.27.0";

/// Where kmp-lsp publishes its releases; the tag of a version is `v<version>`.
pub const KMP_LSP_URL: &str = "https://github.com/Hessesian/kmp-lsp/releases/download";

/// The JetBrains Kotlin LSP version `kargo lsp --server kotlin-lsp` uses.
pub const KOTLIN_LSP_VERSION: &str = "263.6379.0";

/// Where JetBrains publishes the Kotlin LSP's standalone archives.
pub const KOTLIN_LSP_URL: &str = "https://download.jetbrains.com/language-server/kotlin-server";

/// The fwcd kotlin-language-server version `kargo lsp --server kls` uses.
pub const KLS_VERSION: &str = "1.3.13";

/// SHA-256 of the `server.zip` of [`KLS_VERSION`], which GitHub releases don't publish.
pub const KLS_SHA256: &str = "4fe7d71d087b307c7869036171bd9d8c6a4284cd7c25b89098b0a24eb2d9b6d2";

/// The newest JDK kotlin-language-server [`KLS_VERSION`] runs on (its Kotlin 2.1 compiler
/// rejects newer ones). `kargo lsp` runs it on a managed JDK of this version when the
/// project's JDK is newer.
pub const KLS_MAX_JDK: u32 = 21;

/// The main class of kotlin-language-server, started on `java` with its `lib/*` jars.
pub const KLS_MAIN_CLASS: &str = "org.javacs.kt.MainKt";

/// The fwcd kotlin-debug-adapter version `kargo dap` uses.
pub const KDA_VERSION: &str = "0.4.4";

/// SHA-256 of the `adapter.zip` of [`KDA_VERSION`], which GitHub releases don't publish.
pub const KDA_SHA256: &str = "3874cbaded0fdb8229a381167895b0a6caf88b7adffabc690fcf5a6fb65d11b6";

/// The main class of kotlin-debug-adapter, started on `java` with its `lib/*` jars.
pub const KDA_MAIN_CLASS: &str = "org.javacs.ktda.KDAMainKt";

/// A Kotlin language server, or the Kotlin debug adapter, Kargo can download and run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerKind {
    /// kmp-lsp, the default.
    KmpLsp,
    /// JetBrains' Kotlin LSP.
    KotlinLsp,
    /// fwcd's kotlin-language-server.
    Kls,
    /// fwcd's kotlin-debug-adapter, behind `kargo dap`.
    Kda,
}

impl ServerKind {
    pub const ALL: [ServerKind; 4] = [ServerKind::KmpLsp, ServerKind::KotlinLsp, ServerKind::Kls, ServerKind::Kda];

    /// The name used on the command line and as the install directory.
    pub fn as_str(self) -> &'static str {
        match self {
            ServerKind::KmpLsp => "kmp-lsp",
            ServerKind::KotlinLsp => "kotlin-lsp",
            ServerKind::Kls => "kls",
            ServerKind::Kda => "kotlin-debug-adapter",
        }
    }

    /// The human-readable name.
    pub fn title(self) -> &'static str {
        match self {
            ServerKind::KmpLsp => "kmp-lsp",
            ServerKind::KotlinLsp => "Kotlin LSP",
            ServerKind::Kls => "kotlin-language-server",
            ServerKind::Kda => "kotlin-debug-adapter",
        }
    }

    /// The Kargo command that runs it.
    pub fn used_by(self) -> &'static str {
        match self {
            ServerKind::KmpLsp | ServerKind::KotlinLsp | ServerKind::Kls => "kargo lsp",
            ServerKind::Kda => "kargo dap",
        }
    }

    /// The version Kargo pins.
    pub fn default_version(self) -> &'static str {
        match self {
            ServerKind::KmpLsp => KMP_LSP_VERSION,
            ServerKind::KotlinLsp => KOTLIN_LSP_VERSION,
            ServerKind::Kls => KLS_VERSION,
            ServerKind::Kda => KDA_VERSION,
        }
    }

    /// Name of the directory holding the installed versions, under the toolchains directory.
    fn dir_name(self) -> &'static str {
        match self {
            ServerKind::KmpLsp => "kmp-lsp",
            ServerKind::KotlinLsp => "kotlin-lsp",
            ServerKind::Kls => "kotlin-language-server",
            ServerKind::Kda => "kotlin-debug-adapter",
        }
    }

    /// Directory holding the installed versions.
    pub fn dir(self, config: &Config) -> PathBuf {
        config.toolchains_dir().join(self.dir_name())
    }

    /// The launcher, relative to the installation.
    fn launcher(self) -> &'static str {
        match (self, cfg!(windows)) {
            (ServerKind::KmpLsp, true) => "kmp-lsp.exe",
            (ServerKind::KmpLsp, false) => "kmp-lsp",
            (ServerKind::KotlinLsp, true) => "intellij-server.exe",
            (ServerKind::KotlinLsp, false) => "intellij-server",
            (ServerKind::Kls, true) => "kotlin-language-server.bat",
            (ServerKind::Kls, false) => "kotlin-language-server",
            (ServerKind::Kda, true) => "kotlin-debug-adapter.bat",
            (ServerKind::Kda, false) => "kotlin-debug-adapter",
        }
    }

    /// Arguments that make the server talk LSP over stdin and stdout.
    pub fn stdio_args(self) -> &'static [&'static str] {
        match self {
            ServerKind::KotlinLsp => &["--stdio"],
            ServerKind::KmpLsp | ServerKind::Kls | ServerKind::Kda => &[],
        }
    }

    /// Whether the server needs a JDK (`JAVA_HOME`) to run.
    pub fn needs_jdk(self) -> bool {
        matches!(self, ServerKind::Kls | ServerKind::Kda)
    }
}

impl std::str::FromStr for ServerKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "kmp-lsp" => Ok(ServerKind::KmpLsp),
            "kotlin-lsp" => Ok(ServerKind::KotlinLsp),
            "kls" | "kotlin-language-server" => Ok(ServerKind::Kls),
            "kotlin-debug-adapter" => Ok(ServerKind::Kda),
            _ => bail!("unknown Kotlin language server `{s}`; use `kmp-lsp`, `kotlin-lsp` or `kls`"),
        }
    }
}

/// The archive of the Kotlin LSP for this OS and architecture.
fn kotlin_lsp_archive(version: &str) -> Result<(String, ArchiveKind)> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "",
        "aarch64" => "-aarch64",
        other => bail!("the Kotlin LSP is not available for {other} processors; use `--server kmp-lsp` or `--server kls`"),
    };
    Ok(match std::env::consts::OS {
        "windows" => (format!("kotlin-server-{version}{arch}.win.zip"), ArchiveKind::Zip),
        "linux" => (format!("kotlin-server-{version}{arch}.tar.gz"), ArchiveKind::TarGz),
        // The macOS `.sit` archives are zip files.
        "macos" => (format!("kotlin-server-{version}{arch}.sit"), ArchiveKind::Zip),
        other => bail!("the Kotlin LSP is not available for {other}; use `--server kmp-lsp` or `--server kls`"),
    })
}

/// The archives of kmp-lsp for this OS and architecture: on Linux and macOS one tarball holding
/// `kmp-lsp` and its `kmp-jar-indexer` sidecar, on Windows a zip of each.
fn kmp_lsp_archives() -> Result<Vec<(String, ArchiveKind)>> {
    let arch = match std::env::consts::ARCH {
        arch @ ("x86_64" | "aarch64") => arch,
        other => bail!("kmp-lsp is not available for {other} processors; use `--server kls`"),
    };
    Ok(match std::env::consts::OS {
        "windows" => vec![
            (format!("kmp-lsp-windows-{arch}.zip"), ArchiveKind::Zip),
            (format!("kmp-jar-indexer-windows-{arch}.zip"), ArchiveKind::Zip),
        ],
        "linux" => vec![(format!("kmp-lsp-linux-{arch}.tar.gz"), ArchiveKind::TarGz)],
        "macos" => vec![(format!("kmp-lsp-darwin-{arch}.tar.gz"), ArchiveKind::TarGz)],
        other => bail!("kmp-lsp is not available for {other}; use `--server kls`"),
    })
}

/// The SHA-256 `sha256sums.txt` lists for the file `name`.
fn checksum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        let valid = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
        (valid && file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Moves `kmp-lsp` and `kmp-jar-indexer` out of an extracted release archive into `bin`,
/// under the names kmp-lsp looks for (it starts the indexer from the directory it runs from).
/// The release names them after the platform on Windows (`kmp-lsp-windows-x86_64.exe`).
fn place_kmp_binaries(extracted: &Path, bin: &Path, suffix: &str) -> Result<()> {
    std::fs::create_dir_all(bin)?;
    for entry in std::fs::read_dir(extracted)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let target = if name.starts_with("kmp-jar-indexer") {
            "kmp-jar-indexer"
        } else if name.starts_with("kmp-lsp") {
            "kmp-lsp"
        } else {
            continue;
        };
        let to = bin.join(format!("{target}{suffix}"));
        kargo_util::fs::rename_with_retries(&entry.path(), &to).with_context(|| format!("failed to install {}", to.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// An installed Kotlin language server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageServer {
    pub kind: ServerKind,
    pub version: String,
    pub home: PathBuf,
}

impl LanguageServer {
    fn at(config: &Config, kind: ServerKind, version: &str) -> Result<Self> {
        if version.is_empty()
            || !version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            || version.starts_with('.')
        {
            bail!("invalid {} version `{version}`", kind.title());
        }
        Ok(Self { kind, version: version.to_owned(), home: kind.dir(config).join(version) })
    }

    /// The launcher: the first `bin/<launcher>` in the installation (the macOS archive nests
    /// it in an app bundle).
    pub fn executable(&self) -> Option<PathBuf> {
        find_launcher(&self.home, self.kind.launcher(), 4)
    }

    /// The installed server of `kind` and `version`, if present.
    pub fn find(config: &Config, kind: ServerKind, version: &str) -> Result<Option<Self>> {
        let server = Self::at(config, kind, version)?;
        Ok(server.executable().is_some().then_some(server))
    }

    /// Locks the installation of `version` of `kind` against other Kargo processes, after checking the version.
    fn lock(config: &Config, kind: ServerKind, version: &str) -> Result<FileLock> {
        Self::at(config, kind, version)?;
        install_lock(config, &format!("{}-{version}", kind.dir_name()))
    }

    /// Ensures the server is installed, downloading it if needed.
    pub fn ensure(config: &Config, kind: ServerKind, version: &str) -> Result<Self> {
        let _lock = Self::lock(config, kind, version)?;
        if let Some(found) = Self::find(config, kind, version)? {
            return Ok(found);
        }
        let server = Self::at(config, kind, version)?;
        let (url, file, archive_kind, sha256) = match kind {
            ServerKind::KmpLsp => return Self::install_kmp_lsp(config, server),
            ServerKind::KotlinLsp => {
                let (file, archive_kind) = kotlin_lsp_archive(version)?;
                let url = format!("{KOTLIN_LSP_URL}/{version}/{file}");
                let sha256 = crate::download::get_text(config, &format!("{url}.sha256"))
                    .with_context(|| format!("failed to download the Kotlin LSP {version}; is it a released version?"))?;
                let sha256 = sha256.split_whitespace().next().unwrap_or_default().to_owned();
                (url, file, archive_kind, sha256)
            }
            ServerKind::Kls => {
                if version != KLS_VERSION {
                    bail!("Kargo only knows the checksum of kotlin-language-server {KLS_VERSION}; install that version");
                }
                let url = format!("https://github.com/fwcd/kotlin-language-server/releases/download/{version}/server.zip");
                (url, format!("kotlin-language-server-{version}.zip"), ArchiveKind::Zip, KLS_SHA256.to_owned())
            }
            ServerKind::Kda => {
                if version != KDA_VERSION {
                    bail!("Kargo only knows the checksum of kotlin-debug-adapter {KDA_VERSION}; install that version");
                }
                let url = format!("https://github.com/fwcd/kotlin-debug-adapter/releases/download/{version}/adapter.zip");
                (url, format!("kotlin-debug-adapter-{version}.zip"), ArchiveKind::Zip, KDA_SHA256.to_owned())
            }
        };
        if sha256.len() != 64 || !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("unexpected checksum for {} {version} at {url}.sha256", kind.title());
        }
        let archive_path = config.toolchains_dir().join("downloads").join(&file);
        let cached = archive_path.is_file() && crate::download::verify(&archive_path, &sha256).is_ok();
        if !cached {
            crate::download::download(config, &url, &archive_path, Some(&sha256))?;
        }
        config.shell().status("Installing", format!("{} {version}", kind.title()));
        kargo_util::fs::remove_dir_all_if_exists(&server.home)?;
        archive::install(&archive_path, archive_kind, &server.home)?;
        let _ = std::fs::remove_file(&archive_path);
        if server.executable().is_none() {
            bail!("the {} archive at {} has no bin/{}", kind.title(), server.home.display(), kind.launcher());
        }
        Ok(server)
    }

    /// Downloads kmp-lsp and its jar indexer from the GitHub release `v<version>`, checks them
    /// against the release's `sha256sums.txt` and installs them as `bin/kmp-lsp` and
    /// `bin/kmp-jar-indexer`.
    fn install_kmp_lsp(config: &Config, server: Self) -> Result<Self> {
        let version = &server.version;
        let base = format!("{KMP_LSP_URL}/v{version}");
        let sums_url = format!("{base}/sha256sums.txt");
        let sums = crate::download::get_text(config, &sums_url)
            .with_context(|| format!("failed to download kmp-lsp {version}; is it a released version?"))?;
        let downloads = config.toolchains_dir().join("downloads");
        let mut archives = Vec::new();
        for (name, kind) in kmp_lsp_archives()? {
            let sha256 = checksum_for(&sums, &name).with_context(|| format!("{sums_url} lists no checksum for {name}"))?;
            let path = downloads.join(format!("kmp-lsp-{version}-{name}"));
            if !(path.is_file() && crate::download::verify(&path, &sha256).is_ok()) {
                crate::download::download(config, &format!("{base}/{name}"), &path, Some(&sha256))?;
            }
            archives.push((path, kind));
        }
        config.shell().status("Installing", format!("{} {version}", server.kind.title()));
        kargo_util::fs::remove_dir_all_if_exists(&server.home)?;
        let parent = server.home.parent().context("install destination has no parent")?;
        std::fs::create_dir_all(parent)?;
        let staging = parent.join(format!(".staging-kmp-lsp-{version}-{}", std::process::id()));
        let scratch = parent.join(format!(".staging-kmp-lsp-{version}-{}-archive", std::process::id()));
        let result = (|| -> Result<()> {
            for dir in [&staging, &scratch] {
                kargo_util::fs::remove_dir_all_if_exists(dir)?;
            }
            for (path, kind) in &archives {
                kargo_util::fs::remove_dir_all_if_exists(&scratch)?;
                std::fs::create_dir_all(&scratch)?;
                match kind {
                    ArchiveKind::Zip => archive::extract_zip(path, &scratch)?,
                    ArchiveKind::TarGz => archive::extract_tar_gz(path, &scratch)?,
                }
                place_kmp_binaries(&scratch, &staging.join("bin"), if cfg!(windows) { ".exe" } else { "" })?;
            }
            if !staging.join("bin").join(server.kind.launcher()).is_file() {
                bail!("the kmp-lsp archive has no {}", server.kind.launcher());
            }
            kargo_util::fs::rename_with_retries(&staging, &server.home)
                .with_context(|| format!("failed to install into {}", server.home.display()))
        })();
        for dir in [&staging, &scratch] {
            let _ = kargo_util::fs::remove_dir_all_if_exists(dir);
        }
        result?;
        for (path, _) in &archives {
            let _ = std::fs::remove_file(path);
        }
        Ok(server)
    }

    /// Installed servers of every kind, oldest version first.
    pub fn installed(config: &Config) -> Result<Vec<Self>> {
        let mut out = Vec::new();
        for kind in ServerKind::ALL {
            let Ok(entries) = std::fs::read_dir(kind.dir(config)) else { continue };
            let mut servers: Vec<Self> = entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let server = Self::at(config, kind, &name).ok()?;
                    server.executable().is_some().then_some(server)
                })
                .collect();
            servers.sort_by(|a, b| Version::parse(&a.version).cmp(&Version::parse(&b.version)));
            out.extend(servers);
        }
        Ok(out)
    }

    /// Removes an installed server.
    pub fn remove(config: &Config, kind: ServerKind, version: &str) -> Result<Self> {
        let _lock = Self::lock(config, kind, version)?;
        let Some(server) = Self::find(config, kind, version)? else {
            bail!("{} {version} is not installed; see `kargo toolchain list`", kind.title());
        };
        kargo_util::fs::remove_dir_all_if_exists(&server.home)?;
        Ok(server)
    }
}

fn find_launcher(dir: &Path, launcher: &str, depth: usize) -> Option<PathBuf> {
    let direct = dir.join("bin").join(launcher);
    if direct.is_file() {
        return Some(direct);
    }
    if depth == 0 {
        return None;
    }
    let mut children: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    children.sort();
    children.into_iter().find_map(|child| find_launcher(&child, launcher, depth - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::shell::{Shell, Verbosity};

    fn config(dir: &Path) -> Config {
        Config::new(dir.to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true)
    }

    fn fake(config: &Config, kind: ServerKind, version: &str, nested: &str) {
        let bin = kind.dir(config).join(version).join(nested).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(kind.launcher()), "").unwrap();
    }

    #[test]
    fn finds_lists_and_removes_servers() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path());
        assert!(LanguageServer::installed(&config).unwrap().is_empty());
        fake(&config, ServerKind::KmpLsp, KMP_LSP_VERSION, "");
        fake(&config, ServerKind::KotlinLsp, KOTLIN_LSP_VERSION, "Kotlin LSP.app/Contents");
        fake(&config, ServerKind::Kls, KLS_VERSION, "");
        fake(&config, ServerKind::Kda, KDA_VERSION, "adapter");
        std::fs::create_dir_all(ServerKind::KotlinLsp.dir(&config).join("1.0")).unwrap(); // incomplete

        let installed = LanguageServer::installed(&config).unwrap();
        assert_eq!(
            installed.iter().map(|s| (s.kind, s.version.as_str())).collect::<Vec<_>>(),
            [
                (ServerKind::KmpLsp, KMP_LSP_VERSION),
                (ServerKind::KotlinLsp, KOTLIN_LSP_VERSION),
                (ServerKind::Kls, KLS_VERSION),
                (ServerKind::Kda, KDA_VERSION)
            ]
        );
        let err = LanguageServer::ensure(&config, ServerKind::Kda, "0.1.0").unwrap_err().to_string();
        assert!(err.contains("only knows the checksum"), "{err}");
        let lsp = LanguageServer::ensure(&config, ServerKind::KotlinLsp, KOTLIN_LSP_VERSION).unwrap();
        assert!(lsp.executable().unwrap().ends_with(Path::new("Contents").join("bin").join(ServerKind::KotlinLsp.launcher())));

        let err = format!("{:#}", LanguageServer::ensure(&config, ServerKind::KotlinLsp, "1.0").unwrap_err());
        assert!(err.contains("offline"), "{err}");
        let err = LanguageServer::ensure(&config, ServerKind::Kls, "1.0.0").unwrap_err().to_string();
        assert!(err.contains("only knows the checksum"), "{err}");
        assert!(LanguageServer::find(&config, ServerKind::Kls, "../x").is_err());

        LanguageServer::remove(&config, ServerKind::Kls, KLS_VERSION).unwrap();
        assert!(LanguageServer::remove(&config, ServerKind::Kls, KLS_VERSION).is_err());
        assert_eq!(LanguageServer::installed(&config).unwrap().len(), 3);
    }

    #[test]
    fn parses_server_names() {
        assert_eq!("kotlin-lsp".parse::<ServerKind>().unwrap(), ServerKind::KotlinLsp);
        assert_eq!("kls".parse::<ServerKind>().unwrap(), ServerKind::Kls);
        assert_eq!("kmp-lsp".parse::<ServerKind>().unwrap(), ServerKind::KmpLsp);
        assert!("x".parse::<ServerKind>().unwrap_err().to_string().contains("kotlin-lsp"));
        for kind in ServerKind::ALL {
            assert_eq!(kind.as_str().parse::<ServerKind>().unwrap(), kind);
        }
    }

    #[test]
    fn finds_checksums_by_file_name() {
        let hash = "c14e09971ee5f53895aa6bc30242fdbcc3580aae751c39899923aee1a3962492";
        let sums = format!("{hash}  kmp-lsp-linux-x86_64.tar.gz\n{0}  kmp-lsp-linux-x86_64.gz\nnot-a-hash  kmp-lsp-x\n", "ab".repeat(32));
        assert_eq!(checksum_for(&sums, "kmp-lsp-linux-x86_64.tar.gz").as_deref(), Some(hash));
        assert_eq!(checksum_for(&sums, "kmp-lsp-linux-x86_64.gz").as_deref(), Some("ab".repeat(32).as_str()));
        assert_eq!(checksum_for(&sums, "kmp-lsp-x"), None, "a malformed checksum is no checksum");
        assert_eq!(checksum_for(&sums, "kmp-lsp-darwin-aarch64.tar.gz"), None);
    }

    #[test]
    fn places_kmp_lsp_next_to_its_indexer() {
        let tmp = tempfile::tempdir().unwrap();
        let (from, bin) = (tmp.path().join("archive"), tmp.path().join("home").join("bin"));
        std::fs::create_dir_all(&from).unwrap();
        for name in ["kmp-lsp-windows-x86_64.exe", "kmp-jar-indexer-windows-x86_64.exe", "README.md"] {
            std::fs::write(from.join(name), name).unwrap();
        }
        place_kmp_binaries(&from, &bin, ".exe").unwrap();
        let mut placed: Vec<String> =
            std::fs::read_dir(&bin).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        placed.sort();
        assert_eq!(placed, ["kmp-jar-indexer.exe", "kmp-lsp.exe"]);
        assert_eq!(std::fs::read_to_string(bin.join("kmp-lsp.exe")).unwrap(), "kmp-lsp-windows-x86_64.exe");
    }
}
