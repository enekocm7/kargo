//! External toolchains: JDKs (detected or downloaded Temurin builds), GraalVM, the
//! Kotlin compiler, Kotlin/Native, Node.js, web browsers, Gradle and Kotlin language servers.

pub mod adoptium;
pub mod archive;
pub mod browser;
pub mod download;
pub mod graalvm;
pub mod gradle;
pub mod jdk;
pub mod jdks;
pub mod kotlin;
pub mod language_server;
pub mod native;

use std::path::PathBuf;

use anyhow::{Context, Result};

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

/// Locks toolchain `name` (e.g. `jdk-21.0.2+13`) across Kargo processes, for an install or a removal.
///
/// Callers take it before checking whether the toolchain is installed, and hold it until the
/// installation's metadata is written. A second process starting the same install then waits and
/// reuses the result, instead of deleting a directory the first one is still writing.
pub(crate) fn install_lock(config: &Config, name: &str) -> Result<FileLock> {
    FileLock::exclusive(&config.toolchains_dir().join(format!(".lock-{name}")), config.shell(), &format!("toolchain {name}"))
}
