//! Kargo's build engine: workspaces, lockfile, built-in frameworks, tasks,
//! plugins, Android/Gradle integration, warnings about suspicious projects and
//! the high-level operations behind each CLI command.

pub mod builtins;
pub mod dts;
pub mod env;
pub mod gradle;
pub mod lockfile;
pub mod npm;
pub mod ops;
pub mod plugins;
pub mod publication;
pub mod script;
pub mod signing;
pub mod tasks;
pub mod warnings;
pub mod wasm_output;
pub mod web;
pub mod workspace;
