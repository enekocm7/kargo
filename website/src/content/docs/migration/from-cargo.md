---
title: For Rust developers
description: Kargo is modelled on Cargo. Here's how the concepts map.
---

If you know Cargo, you already know most of Kargo. The commands, output style, profiles, lockfile and exit codes are deliberately the same.

## Commands

| Cargo | Kargo | Notes |
|---|---|---|
| `cargo new foo [--lib]` | `kargo new foo [--lib]` | Add `-t jvm,wasm,native,android` and `-w <frameworks>` |
| `cargo init` | `kargo init` | |
| `cargo build [--release]` | `kargo build [--release]` | `-t` picks targets |
| `cargo run -- args` | `kargo run -- args` | |
| `cargo test [filter]` | `kargo test [filter]` | |
| `cargo add serde` | `kargo add group:artifact` | Maven coordinates instead of crate names |
| `cargo add --dev` | `kargo add --dev` | |
| `cargo add -F derive serde` | `kargo add -F serialization` | Kargo's `-F` enables built-in **frameworks** |
| `cargo remove` | `kargo remove` / `kargo rm` | |
| `cargo tree [-d]` | `kargo tree [--duplicates]` | |
| `cargo fetch` / `cargo update` | `kargo fetch` / `kargo update` | |
| `cargo clean` | `kargo clean` | |
| `cargo install` → `cargo foo` | `kargo-foo` on `PATH` → `kargo foo` | Same external subcommand convention |
| `rustup toolchain install` | `kargo toolchain install` | Manages JDKs and Kotlin/Native |
| `--locked`, `--offline`, `-v`, `-q`, `--manifest-path` | same | |

## Files

| Cargo | Kargo |
|---|---|
| `Cargo.toml` | `kargo.toml` |
| `Cargo.lock` | `kargo.lock` |
| `src/main.rs` / `src/lib.rs` | any `.kt` file in `src/` (`fun main` makes it an app) |
| `tests/` | `tests/` (unit tests don't live next to the code) |
| `[target.'cfg(unix)'.dependencies]` | `[target.jvm.dependencies]` etc. |
| `#[cfg(target_os = ...)]` | `expect`/`actual` with `src-<target>/` |
| `[profile.release]` | `[profile.release]` |
| `rust-toolchain.toml` | `[toolchain] jdk = "21"` and `[kotlin] version` |
| `[features]` | no equivalent; `[frameworks]` are the closest thing |
| `build.rs` | a task with `hooks = ["pre-build"]` |
| `crates.io` | Maven Central |

## Concepts that differ

- **Coordinates.** A dependency is `group:artifact`, such as `org.jetbrains.kotlinx:kotlinx-coroutines-core`. Versions are Maven versions, not semver: `"1.2.3"` means "1.2.3 unless something needs newer", and ranges use Maven syntax like `[1.2,2.0)`.
- **Resolution.** When two dependencies need different versions, the highest one wins and only one copy is used. There's no equivalent of Cargo's multiple semver-incompatible copies.
- **Targets are platforms.** `-t jvm` is closer to `--target x86_64-unknown-linux-gnu` than to Cargo's bin/lib targets.
- **Packaging.** `kargo package` builds a fat jar you run with `java -jar`. For a native binary use the [Native target](../../guides/native/).
- **Workspaces.** Kargo itself is a Cargo workspace, but Kargo projects are single packages for now.

## Kotlin for Rustaceans

| Rust | Kotlin |
|---|---|
| `let` / `let mut` | `val` / `var` |
| `Option<T>` | `T?` with `?.`, `?:` and `!!` |
| `Result<T, E>` | exceptions, or `Result<T>` / sealed classes |
| `enum` with data | `sealed interface` + `data class` |
| `match` | `when` |
| `impl Trait for T` | `interface` + `class T : Trait`, or extension functions |
| `#[derive(Debug, Clone, PartialEq)]` | `data class` |
| `#[derive(Serialize)]` | `@Serializable` with the `serialization` framework |
| `async`/`.await` + tokio | `suspend` functions + `kotlinx.coroutines` |
| `Vec<T>` / `HashMap<K, V>` | `List<T>`/`MutableList<T>`, `Map<K, V>`/`MutableMap<K, V>` |
| iterator adapters | the same names: `map`, `filter`, `fold`, `zip`, `windowed`, ... |
