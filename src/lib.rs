//! Kargo: a Cargo-like build tool and package manager for Kotlin.
//!
//! Kargo reads a `kargo.toml` manifest, resolves Maven dependencies, drives the
//! Kotlin compilers (JVM, Wasm and Kotlin/Native) directly and delegates
//! Android builds to a generated Gradle project.

pub mod builtins;
pub mod cli;
pub mod compiler;
pub mod config;
pub mod gradle;
pub mod lockfile;
pub mod manifest;
pub mod maven;
pub mod ops;
pub mod plugins;
pub mod shell;
pub mod tasks;
pub mod toolchain;
pub mod util;
pub mod workspace;
