//! Kotlin language servers, which `kargo lsp` runs behind its own server, and the Kotlin debug
//! adapter `kargo dap` runs behind its own.
//!
//! Each is pinned to one version like the compiler:
//!
//! - Kargo's own Kotlin server (`kargo-kotlin-server`, built from `kotlin-server/` in Kargo's
//!   repository on the Kotlin Analysis API), downloaded from Kargo's GitHub releases and
//!   checked against [`KARGO_KOTLIN_SERVER_SHA256`]. It runs on the project's JDK when that is
//!   [`KARGO_KOTLIN_SERVER_MIN_JDK`] or newer, else on a managed JDK
//!   [`KARGO_KOTLIN_SERVER_MANAGED_JDK`].
//! - [JetBrains' Kotlin LSP](https://github.com/Kotlin/kotlin-lsp) (`kotlin-lsp`, the default),
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

use crate::archive::{self, ArchiveKind};

/// The version of Kargo's own Kotlin server `kargo lsp --server kargo` uses.
pub const KARGO_KOTLIN_SERVER_VERSION: &str = "0.1.0";

/// SHA-256 of `kargo-kotlin-server-<version>.zip` of [`KARGO_KOTLIN_SERVER_VERSION`], or `None`
/// while that version has no release: run a local build with `--server-path` then.
pub const KARGO_KOTLIN_SERVER_SHA256: Option<&str> = None;

/// Where Kargo's GitHub releases are: the server's zip is the asset of the release tagged
/// `kotlin-server-v<version>`.
pub const KARGO_KOTLIN_SERVER_URL: &str = "https://github.com/enekocm7/kargo/releases/download";

/// The main class of Kargo's Kotlin server, started on `java` with its `lib/*` jars.
pub const KARGO_KOTLIN_SERVER_MAIN_CLASS: &str = "io.github.enekocm7.kargo.server.MainKt";

/// The oldest JDK Kargo's Kotlin server runs on (the Kotlin Analysis API needs it).
pub const KARGO_KOTLIN_SERVER_MIN_JDK: u32 = 17;

/// The managed JDK Kargo's Kotlin server runs on when the project's JDK is older than
/// [`KARGO_KOTLIN_SERVER_MIN_JDK`] or unknown.
pub const KARGO_KOTLIN_SERVER_MANAGED_JDK: u32 = 21;

/// The JetBrains Kotlin LSP version `kargo lsp` uses.
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
    /// Kargo's own Kotlin server, on the Kotlin Analysis API.
    Kargo,
    /// JetBrains' Kotlin LSP.
    KotlinLsp,
    /// fwcd's kotlin-language-server.
    Kls,
    /// fwcd's kotlin-debug-adapter, behind `kargo dap`.
    Kda,
}

impl ServerKind {
    pub const ALL: [ServerKind; 4] = [ServerKind::Kargo, ServerKind::KotlinLsp, ServerKind::Kls, ServerKind::Kda];

    /// The name used on the command line (`kargo toolchain`) and in `kargo toolchain list`.
    pub fn as_str(self) -> &'static str {
        match self {
            ServerKind::Kargo => "kargo-kotlin-server",
            ServerKind::KotlinLsp => "kotlin-lsp",
            ServerKind::Kls => "kls",
            ServerKind::Kda => "kotlin-debug-adapter",
        }
    }

    /// The human-readable name.
    pub fn title(self) -> &'static str {
        match self {
            ServerKind::Kargo => "Kargo Kotlin Server",
            ServerKind::KotlinLsp => "Kotlin LSP",
            ServerKind::Kls => "kotlin-language-server",
            ServerKind::Kda => "kotlin-debug-adapter",
        }
    }

    /// The Kargo command that runs it.
    pub fn used_by(self) -> &'static str {
        match self {
            ServerKind::Kargo | ServerKind::KotlinLsp | ServerKind::Kls => "kargo lsp",
            ServerKind::Kda => "kargo dap",
        }
    }

    /// The version Kargo pins.
    pub fn default_version(self) -> &'static str {
        match self {
            ServerKind::Kargo => KARGO_KOTLIN_SERVER_VERSION,
            ServerKind::KotlinLsp => KOTLIN_LSP_VERSION,
            ServerKind::Kls => KLS_VERSION,
            ServerKind::Kda => KDA_VERSION,
        }
    }

    /// Directory holding the installed versions.
    pub fn dir(self, config: &Config) -> PathBuf {
        config.toolchains_dir().join(match self {
            ServerKind::Kargo => "kargo-kotlin-server",
            ServerKind::KotlinLsp => "kotlin-lsp",
            ServerKind::Kls => "kotlin-language-server",
            ServerKind::Kda => "kotlin-debug-adapter",
        })
    }

    /// The launcher, relative to the installation.
    fn launcher(self) -> &'static str {
        match (self, cfg!(windows)) {
            (ServerKind::Kargo, true) => "kargo-kotlin-server.bat",
            (ServerKind::Kargo, false) => "kargo-kotlin-server",
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
            ServerKind::Kargo | ServerKind::Kls | ServerKind::Kda => &[],
        }
    }

    /// Whether the server needs a JDK (`JAVA_HOME`) to run.
    pub fn needs_jdk(self) -> bool {
        matches!(self, ServerKind::Kargo | ServerKind::Kls | ServerKind::Kda)
    }
}

impl std::str::FromStr for ServerKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "kargo" | "kargo-kotlin-server" => Ok(ServerKind::Kargo),
            "kotlin-lsp" => Ok(ServerKind::KotlinLsp),
            "kls" | "kotlin-language-server" => Ok(ServerKind::Kls),
            "kotlin-debug-adapter" => Ok(ServerKind::Kda),
            _ => bail!("unknown Kotlin language server `{s}`; use `kargo`, `kotlin-lsp` or `kls`"),
        }
    }
}

/// The archive of the Kotlin LSP for this OS and architecture.
fn kotlin_lsp_archive(version: &str) -> Result<(String, ArchiveKind)> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "",
        "aarch64" => "-aarch64",
        other => bail!("the Kotlin LSP is not available for {other} processors; use `--server kls`"),
    };
    Ok(match std::env::consts::OS {
        "windows" => (format!("kotlin-server-{version}{arch}.win.zip"), ArchiveKind::Zip),
        "linux" => (format!("kotlin-server-{version}{arch}.tar.gz"), ArchiveKind::TarGz),
        // The macOS `.sit` archives are zip files.
        "macos" => (format!("kotlin-server-{version}{arch}.sit"), ArchiveKind::Zip),
        other => bail!("the Kotlin LSP is not available for {other}; use `--server kls`"),
    })
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

    /// Ensures the server is installed, downloading it if needed.
    pub fn ensure(config: &Config, kind: ServerKind, version: &str) -> Result<Self> {
        if let Some(found) = Self::find(config, kind, version)? {
            return Ok(found);
        }
        let server = Self::at(config, kind, version)?;
        let (url, file, archive_kind, sha256) = match kind {
            ServerKind::Kargo => {
                if version != KARGO_KOTLIN_SERVER_VERSION {
                    bail!("Kargo only knows the checksum of kargo-kotlin-server {KARGO_KOTLIN_SERVER_VERSION}; install that version");
                }
                let Some(sha256) = KARGO_KOTLIN_SERVER_SHA256 else {
                    bail!(
                        "kargo-kotlin-server {version} is not released yet; build it from kotlin-server/ in Kargo's repository \
                         and run `kargo lsp --server kargo --server-path <its bin/kargo-kotlin-server>`"
                    );
                };
                let file = format!("kargo-kotlin-server-{version}.zip");
                let url = format!("{KARGO_KOTLIN_SERVER_URL}/kotlin-server-v{version}/{file}");
                (url, file, ArchiveKind::Zip, sha256.to_owned())
            }
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
        fake(&config, ServerKind::KotlinLsp, KOTLIN_LSP_VERSION, "Kotlin LSP.app/Contents");
        fake(&config, ServerKind::Kls, KLS_VERSION, "");
        fake(&config, ServerKind::Kda, KDA_VERSION, "adapter");
        std::fs::create_dir_all(ServerKind::KotlinLsp.dir(&config).join("1.0")).unwrap(); // incomplete

        let installed = LanguageServer::installed(&config).unwrap();
        assert_eq!(
            installed.iter().map(|s| (s.kind, s.version.as_str())).collect::<Vec<_>>(),
            [(ServerKind::KotlinLsp, KOTLIN_LSP_VERSION), (ServerKind::Kls, KLS_VERSION), (ServerKind::Kda, KDA_VERSION)]
        );
        let err = LanguageServer::ensure(&config, ServerKind::Kda, "0.1.0").unwrap_err().to_string();
        assert!(err.contains("only knows the checksum"), "{err}");
        let lsp = LanguageServer::ensure(&config, ServerKind::KotlinLsp, KOTLIN_LSP_VERSION).unwrap();
        assert!(lsp.executable().unwrap().ends_with(Path::new("Contents").join("bin").join(ServerKind::KotlinLsp.launcher())));

        let err = format!("{:#}", LanguageServer::ensure(&config, ServerKind::KotlinLsp, "1.0").unwrap_err());
        assert!(err.contains("offline"), "{err}");
        let err = LanguageServer::ensure(&config, ServerKind::Kls, "1.0.0").unwrap_err().to_string();
        assert!(err.contains("only knows the checksum"), "{err}");
        let err = format!("{:#}", LanguageServer::ensure(&config, ServerKind::Kargo, KARGO_KOTLIN_SERVER_VERSION).unwrap_err());
        let expected = if KARGO_KOTLIN_SERVER_SHA256.is_some() { "offline" } else { "--server-path" };
        assert!(err.contains(expected), "{err}");
        fake(&config, ServerKind::Kargo, KARGO_KOTLIN_SERVER_VERSION, "kargo-kotlin-server-0.1.0");
        assert!(LanguageServer::find(&config, ServerKind::Kargo, KARGO_KOTLIN_SERVER_VERSION).unwrap().is_some());
        LanguageServer::remove(&config, ServerKind::Kargo, KARGO_KOTLIN_SERVER_VERSION).unwrap();
        assert!(LanguageServer::find(&config, ServerKind::Kls, "../x").is_err());

        LanguageServer::remove(&config, ServerKind::Kls, KLS_VERSION).unwrap();
        assert!(LanguageServer::remove(&config, ServerKind::Kls, KLS_VERSION).is_err());
        assert_eq!(LanguageServer::installed(&config).unwrap().len(), 2);
    }

    #[test]
    fn parses_server_names() {
        assert_eq!("kotlin-lsp".parse::<ServerKind>().unwrap(), ServerKind::KotlinLsp);
        assert_eq!("kls".parse::<ServerKind>().unwrap(), ServerKind::Kls);
        assert_eq!("kargo".parse::<ServerKind>().unwrap(), ServerKind::Kargo);
        assert!("x".parse::<ServerKind>().unwrap_err().to_string().contains("kotlin-lsp"));
        for kind in ServerKind::ALL {
            assert_eq!(kind.as_str().parse::<ServerKind>().unwrap(), kind);
        }
    }
}
