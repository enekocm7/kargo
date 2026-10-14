//! Kargo's own Kotlin language server: `kargo lsp --server kargo`, the default.
//!
//! It is written in Rust and runs inside `kargo lsp`, so it needs no JDK and starts at once. It
//! reads the project's sources as Kargo lays them out (see [`server::Model`]) and the standard
//! library from its sources jar ([`library`]), and offers completion, hover, go to definition,
//! references and rename ([`features`]) on top of a declaration parser ([`syntax`]) and a
//! resolver with enough type inference for member access ([`resolve`]).

pub mod features;
pub mod index;
pub mod lexer;
pub mod library;
pub mod resolve;
pub mod server;
pub mod syntax;

pub use server::spawn;

#[cfg(test)]
mod tests;
