//! External toolchains: JDKs (detected or downloaded Temurin builds), GraalVM, the
//! Kotlin compiler, Kotlin/Native, Node.js, web browsers, Gradle, the Android SDK and Kotlin language servers.

pub mod adoptium;
pub mod android_sdk;
pub mod archive;
pub mod binaryen;
pub mod browser;
pub mod download;
pub mod graalvm;
pub mod gradle;
pub mod jbr;
pub mod jdk;
pub mod jdks;
pub mod kotlin;
pub mod language_server;
pub mod native;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

pub use graalvm::Graalvm;
pub use gradle::GradleDistribution;
pub use jdk::Jdk;
pub use kotlin::KotlinCompiler;
pub use language_server::{LanguageServer, ServerKind};
pub use native::NativeToolchain;

use kargo_util::config::Config;
use kargo_util::flock::FileLock;
use kargo_util::process::find_on_path;

/// Finds Node.js, used to run Kotlin/Wasm (`js` and `wasi`) output.
pub fn node() -> Result<PathBuf> {
    find_on_path("node").context("Node.js is required to run Kotlin/Wasm programs; install it and put `node` on PATH")
}

/// Fails when the user turned automatic installs off (`KARGO_AUTO_INSTALL=0`, `[toolchain]
/// auto-install = false`) and a command needs `what`, a toolchain it doesn't find installed.
/// `install` is the command that installs it by hand. Offline mode is left to the download,
/// which a mirror or vendored repository on disk can still serve.
pub fn ensure_auto_install(config: &Config, what: &str, install: &str) -> Result<()> {
    if !config.auto_install() {
        bail!("{what} is not installed and {AUTO_INSTALL_OFF}; run `{install}`");
    }
    Ok(())
}

/// Why [`ensure_auto_install`] refuses when the user turned automatic installs off.
pub(crate) const AUTO_INSTALL_OFF: &str =
    "automatic toolchain installs are off (KARGO_AUTO_INSTALL=0, or [toolchain] auto-install = false in config.toml)";

/// Locks toolchain `name` (e.g. `jdk-21.0.2+13`) across Kargo processes, for an install or a removal.
///
/// Callers take it before checking whether the toolchain is installed, and hold it until the
/// installation's metadata is written. A second process starting the same install then waits and
/// reuses the result, instead of deleting a directory the first one is still writing.
pub(crate) fn install_lock(config: &Config, name: &str) -> Result<FileLock> {
    FileLock::exclusive(&config.toolchains_dir().join(format!(".lock-{name}")), config.shell(), &format!("toolchain {name}"))
}
