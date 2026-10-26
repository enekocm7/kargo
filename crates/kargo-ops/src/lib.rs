//! Kargo's build engine: workspaces, lockfile, built-in frameworks, tasks,
//! plugins, Android/Gradle integration, warnings about suspicious projects and
//! the high-level operations behind each CLI command.

pub mod bench;
pub mod builtins;
pub mod compose;
pub mod dts;
pub mod env;
pub mod generate;
pub mod gradle;
pub mod js_output;
pub mod js_sources;
pub mod lockfile;
pub mod native_interop;
pub mod npm;
pub mod ops;
pub mod pkg_config;
pub mod plugins;
pub mod publication;
pub mod script;
pub mod signing;
pub mod tasks;
pub mod warnings;
pub mod wasm_output;
pub mod web;
pub mod web_resources;
pub mod workspace;
