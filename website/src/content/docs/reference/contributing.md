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
| `kargo-maven` | Coordinates, Maven versions and ranges, POMs, Gradle module metadata, downloads, the resolver, uploads (`deploy.rs`) and the Maven Central Portal (`central.rs`) |
| `kargo-manifest` | The `kargo.toml` model, validation and format-preserving edits |
| `kargo-toolchain` | JDK detection and Temurin downloads, the Kotlin compiler, Kotlin/Native, Gradle distributions, Kotlin language servers and the Kotlin debug adapter, browsers, archives |
| `kargo-compiler` | JVM/Wasm/Native compiler drivers, the compiler daemon, incremental compilation, ABI fingerprints of jars, jar packaging, entry points, test reports, compiler diagnostics |
| `kargo-ops` | Build engine, lockfile, frameworks, tasks, plugins, Android/Gradle, project warnings, publications (`publication.rs`) and GnuPG signing (`signing.rs`), commands |
| `kargo-lsp` | `kargo lsp`: the message loop in front of a Kotlin language server (`server.rs`), `kargo.toml` diagnostics, completion and hover (`manifest.rs`, `schema.rs`), Maven coordinate and version completion and newer-version hints (`deps.rs`), code actions and the `kargo/addDependency` / `kargo/addTarget` commands (`actions.rs`), `workspace.json`, `kls-classpath` and runnables from `kargo metadata` (`project.rs`), check on save (`check.rs`), the `kargo dap` debug adapter (`dap.rs`), Zed tasks for `kargo ide --zed` (`zed.rs`) |
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

## The JetBrains plugin

The plugin for IntelliJ IDEA and Android Studio ([IntelliJ IDEA and Android Studio](../../guides/editors/#intellij-idea-and-android-studio)) is a Gradle project written in Kotlin, in `editors/jetbrains`. It imports the project model from `kargo metadata`, runs `kargo lsp` through [LSP4IJ](https://github.com/redhat-developer/lsp4ij) and adds the IDE parts around it:

| Package | Responsibility |
|---|---|
| `project` | The project import: `KargoMetadata` reads `kargo metadata`, `KargoImportPlanner` turns it into modules, roots and libraries (plain data, unit-tested), `KargoProjectImporter` applies the plan to the IDE, `KotlinSupport` is the only code that touches the Kotlin plugin's facets and library kinds, `KargoProjectSync` and `KargoFileListener` decide when to import |
| `lsp` | Starting `kargo lsp` with the settings' options, the `kargo/runnables` request and the `kargo/addDependency` / `kargo/addTarget` commands |
| `run` | The Kargo run configuration, gutter icons, the test tree (`--message-format json` to test events), debugging through `--debug` |
| `wizard` | *File → New → Project → Kargo* (`kargo init`) |
| `actions`, `toolwindow`, `settings` | *Tools → Kargo*, the Kargo tool window, *Settings → Languages & Frameworks → Kargo* |

```sh
cd editors/jetbrains
./gradlew buildPlugin test     # the plugin zip in build/distributions
./gradlew runIde               # an IDE with the plugin installed
./gradlew verifyPlugin         # check compatibility with recent IDEs
./gradlew verifyPlugin -PverifyIde="<an installed IDE>"   # ... or with the IDE you have
```

It needs JDK 21. The *JetBrains plugin* workflow builds and verifies it on every change to `editors/jetbrains`.

The import tests (`KargoImportTest`) run the IDE's Kotlin analysis against imported modules. The Kotlin/Native and multiplatform ones need a Kotlin/Native distribution the IDE under test can read (a `kotlin-native-prebuilt-*` directory with Kotlin 2.3 in `~/.konan`, or in `KONAN_DATA_DIR`) and are skipped without one; `KargoSyncTest` needs `kargo` and skips without it. The plugin uses the Kotlin plugin's facets and library kinds, which are not a stable API, so `verifyPlugin` against the newest IDE is how you find out that a release changed them.

## The Zed extension

The [Zed](../../guides/editors/#zed) extension in `editors/zed` is a Rust crate of its own (not a member of Kargo's workspace), compiled to WebAssembly against [`zed_extension_api`](https://crates.io/crates/zed_extension_api). `extension.toml` declares the language server for Kotlin and TOML, the `Kargo` debug adapter (its configuration schema is `debug_adapter_schemas/Kargo.json`), the `kargo` debug locator and the `/kargo-metadata` slash command; `src/lib.rs` implements them by running `kargo lsp`, `kargo dap` and `kargo metadata`.

```sh
cd editors/zed
cargo test                                     # unit tests, on the host
cargo build --release --target wasm32-wasip2   # the extension Zed builds
```

To try it, run *zed: install dev extension* in Zed and pick `editors/zed`. The *Zed extension* workflow formats, tests, builds and lints it on every change to `editors/zed`.

## This website

The docs are an [Astro](https://astro.build) + [Starlight](https://starlight.astro.build) site in `website/`:

```sh
cd website
npm install
npm run dev        # http://localhost:4321/
npm run build      # static site in website/dist
```

Pages are Markdown files in `website/src/content/docs/`. The sidebar is configured in `website/astro.config.mjs`.
