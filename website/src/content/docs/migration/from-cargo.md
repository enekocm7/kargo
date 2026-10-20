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
| `cargo build [--release] [--keep-going]` | `kargo build [--release] [--keep-going]` | `-t` picks targets; `--keep-going` builds every target despite failures |
| `cargo check [--tests] [--keep-going]` | `kargo check [--tests] [--keep-going]` | Skips packaging and linking; JVM still writes classes |
| `cargo run -- args` | `kargo run -- args` | |
| `cargo run --bin foo` / `--example foo` | `kargo run --bin foo` / `--example foo` | Also `kargo run --main <class>` for an undeclared `main` |
| `cargo test [filter] [--no-fail-fast]` | `kargo test [filter] [--no-fail-fast]` | Fails fast across targets by default; `[test] fail-fast = false` changes that |
| `cargo doc [--open]` | `kargo doc [--open]` | Runs Dokka on your KDoc comments |
| `cargo fmt [--check]` | `kargo fmt [--check]` | Runs ktlint; configured by `.editorconfig` |
| `cargo clippy` | `kargo lint` | Runs detekt; configured by `detekt.yml` |
| `cargo add serde` | `kargo add group:artifact` | Maven coordinates instead of crate names |
| `cargo add --dev` | `kargo add --dev` | |
| `cargo add -F derive serde` | `kargo add -F serialization` | Kargo's `-F` enables built-in **frameworks** |
| `cargo remove` | `kargo remove` / `kargo rm` | |
| `cargo tree [-d]` | `kargo tree [--duplicates]` | |
| `cargo fetch` | `kargo fetch` | |
| `cargo vendor`, `[source]` replacement in `.cargo/config.toml` | `kargo vendor`, `--vendored` or `[source] vendored = true` in `.kargo/config.toml` | `vendor/` is a Maven repository. Mirrors are `[mirrors]` in `~/.kargo/config.toml`; see [Offline and corporate builds](../../guides/offline-and-mirrors/) |
| `cargo update [-p foo] [--precise v] [--dry-run]` | `kargo update [group:artifact] [--precise v] [--dry-run]` | |
| `cargo outdated` (plugin) | `kargo outdated` | Built in |
| `cargo upgrade [--incompatible]` (cargo-edit) | `kargo upgrade [--incompatible]` | Built in; "compatible" means the same major version (minor for `0.x`) |
| `cargo clean` | `kargo clean` | |
| `cargo package` | `kargo package` | A library gets its own jar and sources jar; an application a runnable fat jar |
| `cargo publish [--dry-run]` | `kargo publish [--dry-run]` | To Maven Central, a Maven repository or `~/.m2` (`--local`); see [Publishing a library](../../guides/publishing/) |
| `cargo install --path . / --git <url> / <crate>` | `kargo install --path . / --git <url> / group:artifact` | Launchers go to `~/.kargo/bin`; see [Installing command-line tools](../../guides/installing-tools/) |
| `cargo install --list`, `cargo uninstall` | `kargo install --list`, `kargo uninstall` | |
| `cargo foo` | `kargo foo` (runs `kargo-foo` on `PATH`) | Same external subcommand convention |
| `rustup toolchain install` | `kargo toolchain install` | Manages JDKs and Kotlin/Native |
| `--locked`, `--offline`, `-v`, `-q`, `--manifest-path` | same | |

## Files

| Cargo | Kargo |
|---|---|
| `Cargo.toml` | `kargo.toml` |
| `Cargo.lock` | `kargo.lock` |
| `src/main.rs` / `src/lib.rs` | any `.kt` file in `src/` (`fun main` makes it an app) |
| `[[bin]]` / `src/bin/*.rs` | `[[bin]]` with `path` or `main`; sources stay in `src/` |
| `examples/*.rs` | `examples/*.kt` |
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
- **Workspaces.** `[workspace]` with `members` (globs included), `exclude`, `default-members`, `[workspace.package]` and `[workspace.dependencies]` with `key.workspace = true`, `{ path = "..." }` dependencies, `-p`/`--workspace`/`--exclude` and `-j` work as in Cargo, and `kargo new` inside a workspace adds the member. Kargo adds `[workspace.kotlin]`, `[workspace.frameworks]`, `[workspace.plugins]` and `[workspace.tasks]`, and `exported = true` for dependencies that are part of a package's API. A workspace root can't also be a package. See [Workspaces and path dependencies](../../guides/workspaces/).

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
