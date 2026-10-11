---
title: Contributing
description: How Kargo is built and how to work on it.
---

Kargo is written in Rust and lives at [github.com/enekocm7/kargo](https://github.com/enekocm7/kargo). Issues and pull requests are welcome.

## Architecture

Kargo is a Cargo workspace of focused crates:

| Crate | Responsibility |
|---|---|
| `kargo-util` | Terminal output, configuration, filesystem/hash/process helpers |
| `kargo-maven` | Coordinates, Maven versions and ranges, POMs, Gradle module metadata, downloads, the resolver |
| `kargo-manifest` | The `kargo.toml` model, validation and format-preserving edits |
| `kargo-toolchain` | JDK detection and Temurin downloads, the Kotlin compiler, Kotlin/Native, archives |
| `kargo-compiler` | JVM/Wasm/Native compiler drivers, jar packaging, entry points, test reports |
| `kargo-ops` | Build engine, lockfile, frameworks, tasks, plugins, Android/Gradle, commands |
| `kargo` | The CLI binary |

## Building and testing

```sh
git clone https://github.com/enekocm7/kargo && cd kargo
cargo build
cargo test --workspace                       # unit + CLI integration tests (offline)
KARGO_E2E=1 cargo test --test e2e            # end-to-end: JVM, Wasm, Spring, Ktor, Compose, pinned JDK
KARGO_E2E=1 KARGO_E2E_NATIVE=1 KARGO_E2E_ANDROID=1 cargo test --test e2e
```

Format with `cargo fmt` before committing.

## This website

The docs are an [Astro](https://astro.build) + [Starlight](https://starlight.astro.build) site in `website/`:

```sh
cd website
npm install
npm run dev        # http://localhost:4321/kargo/
npm run build      # static site in website/dist
```

Pages are Markdown files in `website/src/content/docs/`. The sidebar is configured in `website/astro.config.mjs`.
