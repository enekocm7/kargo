//! External toolchains: JDKs (detected or downloaded Temurin builds), the
//! Kotlin compiler, Kotlin/Native, Node.js, web browsers, Gradle and Kotlin language servers.

pub mod adoptium;
pub mod archive;
pub mod browser;
pub mod download;
pub mod gradle;
pub mod jdk;
pub mod jdks;
pub mod kotlin;
pub mod language_server;
pub mod native;

use std::path::PathBuf;

use anyhow::{Context, Result};

pub use gradle::GradleDistribution;
pub use jdk::Jdk;
pub use kotlin::KotlinCompiler;
pub use language_server::{LanguageServer, ServerKind};
pub use native::NativeToolchain;

use kargo_util::process::find_on_path;

/// Finds Node.js, used to run Kotlin/Wasm (`js` and `wasi`) output.
pub fn node() -> Result<PathBuf> {
    find_on_path("node").context("Node.js is required to run Kotlin/Wasm programs; install it and put `node` on PATH")
}
