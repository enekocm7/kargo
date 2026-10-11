//! External toolchains: the JDK, the Kotlin compiler, Kotlin/Native, Node.js and Gradle.

pub mod jdk;
pub mod kotlin;
pub mod native;

use std::path::PathBuf;

use anyhow::{Context, Result};

pub use jdk::Jdk;
pub use kotlin::KotlinCompiler;
pub use native::NativeToolchain;

use crate::util::process::find_on_path;

/// Finds Node.js, used to run Kotlin/Wasm (`js` and `wasi`) output.
pub fn node() -> Result<PathBuf> {
    find_on_path("node").context("Node.js is required to run Kotlin/Wasm programs; install it and put `node` on PATH")
}

/// Finds Gradle, used for Android builds. A `gradlew` in the project wins.
pub fn gradle(project_dir: &std::path::Path) -> Result<PathBuf> {
    let wrapper = project_dir.join(if cfg!(windows) { "gradlew.bat" } else { "gradlew" });
    if wrapper.is_file() {
        return Ok(wrapper);
    }
    find_on_path("gradle").context(
        "Gradle is required for Android builds; install it (https://gradle.org/install) or add a Gradle wrapper (gradlew) to the project",
    )
}
