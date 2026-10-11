//! Kargo
pub mod builtins;
pub mod config;
pub mod lockfile;
pub mod manifest;
pub mod maven;
pub mod shell;
pub mod toolchain;
pub mod util;
pub mod workspace;
pub mod cli { pub fn main() -> std::process::ExitCode { std::process::ExitCode::SUCCESS } }
