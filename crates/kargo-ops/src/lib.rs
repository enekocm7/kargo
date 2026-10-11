//! Kargo's build engine: workspaces, lockfile, built-in frameworks, tasks,
//! plugins, Android/Gradle integration and the high-level operations behind
//! each CLI command.

pub mod builtins;
pub mod gradle;
pub mod lockfile;
pub mod ops;
pub mod plugins;
pub mod tasks;
pub mod workspace;
