//! Kargo's own Kotlin language server: `kargo lsp --server kargo`, the default.
//!
//! It is written in Rust and runs inside `kargo lsp`, so it needs no JDK and starts at once. It
//! reads the project's sources as Kargo lays them out (see [`server::Model`]) and the standard
//! library from its sources jar ([`library`]), and offers completion, hover, go to definition,
//! references and rename ([`features`]), implementations and the call and type hierarchies ([`hierarchy`]), semantic tokens ([`semantic`]), folding and selection ranges ([`ranges`]), and code actions ([`actions`]) on top of a declaration parser ([`syntax`]) and a
//! resolver with enough type inference for member access ([`resolve`]). Java declarations come
//! from the JDK's sources ([`jdk`]) and from `.java` files ([`java`]). Libraries without a
//! sources jar are read from their class files and klibs ([`compiled`]).

pub mod actions;
pub mod classfile;
pub mod compiled;
pub mod diagnostics;
pub mod features;
pub mod hierarchy;
pub mod index;
pub mod java;
pub mod jdk;
pub mod kmetadata;
pub mod lexer;
pub mod library;
pub mod ranges;
pub mod resolve;
pub mod semantic;
pub mod server;
pub mod syntax;

pub use server::spawn;

#[cfg(test)]
mod tests;
