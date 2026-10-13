//! `kargo lsp`: one language server for a Kargo project, over stdio.
//!
//! Kargo answers everything about `kargo.toml` itself (diagnostics from its JSON Schema and
//! Kargo's own validation, completion, hover, code actions) and the `kargo/runnables` request, and passes
//! Kotlin traffic to a Kotlin language server it downloads and pins like a toolchain. It
//! describes the project to that server from `kargo metadata` and describes it again when
//! `kargo.toml` or `kargo.lock` change. With `checkOnSave`, it runs `kargo check` when a Kotlin
//! file is saved and merges the diagnostics of every target into the Kotlin server's.

pub mod actions;
pub mod check;
pub mod deps;
pub mod jsonrpc;
pub mod manifest;
pub mod project;
pub mod schema;
pub mod server;
pub mod text;

pub use server::{Options, run, serve};
