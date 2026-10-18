//! `kargo lsp`: one language server for a Kargo project, over stdio.
//!
//! Kargo answers everything about `kargo.toml` itself (diagnostics from its JSON Schema and
//! Kargo's own validation, completion, hover, code actions) and the `kargo/runnables` request, and passes
//! Kotlin traffic to a Kotlin language server. By default that is Kargo's own (see [`kotlin`]),
//! written in Rust and run in-process. It can instead be JetBrains' Kotlin LSP or fwcd's
//! kotlin-language-server, which Kargo downloads and pins like a toolchain, describes the project
//! to from `kargo metadata`, and describes it again when `kargo.toml` or `kargo.lock` change.
//! With `checkOnSave` (on by default with Kargo's own server), it runs `kargo check` when a Kotlin
//! file is saved and merges the diagnostics of every target into the Kotlin server's.
//!
//! `kargo dap` (see [`dap`]) is the debug adapter next to it: kotlin-debug-adapter behind a
//! proxy that launches `kargo run` and `kargo test` for debugging and maps Kargo's source layout.

pub mod actions;
pub mod check;
pub mod dap;
pub mod deps;
pub mod jsonrpc;
pub mod kotlin;
pub mod manifest;
pub mod project;
pub mod schema;
pub mod script;
pub mod server;
pub mod text;
pub mod zed;

pub use server::{KotlinServer, Options, run, serve};
