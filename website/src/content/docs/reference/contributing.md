---
title: Contributing
description: How Kargo is built and how to work on it.
---

Kargo is written in Rust and lives at [github.com/enekocm7/kargo](https://github.com/enekocm7/kargo). Issues and pull requests are welcome.

## Architecture

Kargo is a Cargo workspace of focused crates:

| Crate | Responsibility |
|---|---|
| `kargo-util` | Terminal output, configuration (including `config.toml`: mirrors, proxy, CA bundle, vendoring), the HTTP agent, filesystem/hash/process helpers |
| `kargo-maven` | Coordinates, Maven versions and ranges, POMs, Gradle module metadata, downloads, the resolver, uploads (`deploy.rs`) and the Maven Central Portal (`central.rs`) |
| `kargo-manifest` | The `kargo.toml` model, validation and format-preserving edits |
| `kargo-toolchain` | JDK detection and Temurin downloads, GraalVM downloads and the reachability metadata repository, the Kotlin compiler, Kotlin/Native, Gradle distributions, Kotlin language servers and the Kotlin debug adapter, browsers, archives |
| `kargo-compiler` | JVM/Wasm/Native compiler drivers, the compiler daemon, incremental compilation, ABI fingerprints of jars, jar packaging, entry points, test reports, compiler diagnostics |
| `kargo-ops` | Build engine, lockfile, frameworks, tasks, plugins, `[[generate]]` steps (`generate.rs`), Android/Gradle, project warnings, publications (`publication.rs`) and GnuPG signing (`signing.rs`), npm installs (`npm.rs`), `kargo self` (`ops/self_update.rs`), `kargo import` (`ops/import/`), Kotlin declarations from TypeScript ones (`dts/`), C interop of the Native target (`native_interop.rs`, `pkg_config.rs`), Compose Desktop and the `Res` class of Compose resources (`compose/`), the benchmark results, their comparison and report (`bench.rs`), commands |
| `kargo-lsp` | `kargo lsp`: the message loop in front of a Kotlin language server (`server.rs`), Kargo's own Kotlin server (`kotlin/`: the lexer, a parser for declarations and scopes in `syntax.rs`, the indexes in `index.rs`, name resolution and type inference in `resolve.rs`, completion, hover, navigation and rename in `features.rs`, code actions for Kotlin files in `actions.rs`, the standard library and sources jars in `library.rs`, libraries without sources as generated stubs in `compiled.rs`, read from class files in `classfile.rs` and from Kotlin metadata and klibs in `kmetadata.rs`, its message loop in `server.rs`), `kargo.toml` diagnostics, completion and hover (`manifest.rs`, `schema.rs`), Maven coordinate and version completion and newer-version hints (`deps.rs`), code actions and the `kargo/addDependency` / `kargo/addTarget` commands (`actions.rs`), `workspace.json`, `kls-classpath` and runnables from `kargo metadata` (`project.rs`), check on save (`check.rs`), the `kargo dap` debug adapter (`dap.rs`), Zed tasks for `kargo ide --zed` (`zed.rs`) |
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

## Releases

Pushing a tag `v<version>` runs `.github/workflows/release.yml`. The version in the tag must equal `version` in the `[workspace.package]` table of `Cargo.toml`, or the workflow stops. It builds Kargo with `cargo build --release --locked` for eight targets (`x86_64` and `aarch64` for Linux with glibc and with musl, macOS and Windows), runs each binary's `--version`, and publishes a GitHub release with:

| File | Contents |
|---|---|
| `kargo-<version>-<target>.tar.gz` (`.zip` for Windows targets) | A folder of the same name with `kargo` (`kargo.exe`) and the README |
| `SHA256SUMS` | The SHA-256 of every archive, sorted by name |
| `VERSION` | The version; `releases/latest/download/VERSION` is how `kargo self update`, `install.sh` and `install.ps1` find the newest release |

The release notes are generated from the merged pull requests. The archives get GitHub build provenance attestations (`gh attestation verify <archive> --repo enekocm7/kargo`). A version with a hyphen (`0.3.0-rc.1`) is published as a pre-release, which `releases/latest` skips. A pull request that changes the workflow or `packaging/` builds every target and uploads the archives as workflow artifacts without publishing.

These names are used by `crates/kargo-ops/src/ops/self_update.rs` (`archive_name`, unit tested), `website/public/install.sh` and `install.ps1`, the `cargo binstall` metadata in `Cargo.toml`, and the templates in `packaging/`. Change them together.

`packaging/render.sh <version> <SHA256SUMS> <dir>` fills the templates for Homebrew (`kargo.rb`), Scoop (`kargo.json`), winget (three manifests) and the AUR (`PKGBUILD` of `kargo-bin`) from a release's checksums; the workflow uploads the result as the `packages` artifact. Copy the files to the tap, the bucket, a winget-pkgs pull request and the AUR repository by hand. The root `flake.nix` builds Kargo from source for Nix.

Artifacts are not signed with a key of ours: `kargo self update` and the install scripts verify the SHA-256 from the same release, which catches corrupt and swapped files but not a compromised release. For that, verify the attestation.

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

## The VS Code extension

The [Emacs](../../guides/editors/#emacs) package in `editors/emacs` is Emacs Lisp for Emacs 29.1 or newer. `kargo.el` runs the commands through `compile`, `kargo-messages.el` reads their `--message-format json` output, `kargo-test.el` runs tests and lists the results, and `kargo-eglot.el`, `lsp-kargo.el`, `kargo-flymake.el`, `kargo-dape.el` and `kargo-transient.el` connect Eglot, lsp-mode, Flymake, Dape and Transient. `test/kargo-tests.el` runs with `emacs -Q --batch -L . -l test/kargo-tests.el -f ert-run-tests-batch-and-exit`: it needs no Kargo binary, since the messages are fixtures and the tests that run a command use a shell script that prints one, and it tests the lsp-mode and Dape files when those packages are installed. An Emacs workflow byte-compiles the package and runs the tests on every change to `editors/emacs`.

The [Neovim](../../guides/editors/#neovim) plugin in `editors/neovim` is plain Lua for Neovim 0.11 or newer, with no plugin dependencies of its own. `lsp/kargo.lua` is the server config, `lua/kargo/` the `:Kargo` command and its nvim-dap setup, and `lua/neotest-kargo/` the neotest adapter. `tests/run.lua` runs with `nvim --headless -u NONE -l tests/run.lua`: it stubs neotest, so it needs neither neotest nor a Kargo binary, and it reads fixture Kotlin sources and `--message-format json` output. A Neovim workflow runs the tests on every change to `editors/neovim`.

The [Visual Studio Code](../../guides/editors/#visual-studio-code) extension in `editors/vscode` is a TypeScript project built with `tsc` against `@types/vscode`, using `vscode-languageclient` for `kargo lsp`. `src/extension.ts` activates in a folder with a `kargo.toml`, and each module does one part: `servers.ts` (the language server), `tasks.ts` (the `kargo` tasks and their terminal, which reads the compiler messages), `testing.ts` (the test explorer), `codeLens.ts` and `kotlinScan.ts` (the Run and Debug buttons), `debug.ts` (`kargo dap`), `actions.ts` and `selection.ts` (the commands and the status bar), `project.ts` and `metadata.ts` (`kargo metadata --no-deps`), and `maven.ts` (the dependency search).

The modules that don't import `vscode` (`commands.ts`, `diagnostics.ts`, `executable.ts`, `kotlinScan.ts`, `maven.ts`, `messages.ts`, `metadata.ts`, `process.ts`) carry the logic and are unit tested with `node:test`. Changing a command line goes through `commands.ts`; changing a JSON message goes through `messages.ts`. Kotlin's colours come from the TextMate grammar in `syntaxes/kotlin.tmLanguage.json`, which `kotlinGrammar.test.ts` checks by tokenizing sample code with the same engine VS Code uses.

```sh
cd editors/vscode
npm ci
npm test                    # unit tests
npm run test:integration    # VS Code 1.100 with the extension and a new project: needs kargo on PATH and network
npm run package             # kargo-vscode.vsix
```

`npm run test:integration` downloads VS Code the first time, creates a project with `kargo new`, and checks in the extension host that the commands are registered, the tasks are offered, `kargo lsp` runs, the tests are discovered and run with their results, a build succeeds, a compiler error reaches the Problems view, and the Run and Debug buttons appear. Run it after a change to how the extension starts `kargo` or reads its output. The *VS Code extension* workflow runs the unit tests and builds the package on every change to `editors/vscode`.

## This website

The docs are an [Astro](https://astro.build) + [Starlight](https://starlight.astro.build) site in `website/`:

```sh
cd website
npm install
npm run dev        # http://localhost:4321/
npm run build      # static site in website/dist
```

Pages are Markdown files in `website/src/content/docs/`. The sidebar is configured in `website/astro.config.mjs`.
