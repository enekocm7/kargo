# AGENTS.md

Guidance for AI coding agents (and humans) working in this repository. `CLAUDE.md` imports this file, so this is the single source of truth.

## What Kargo is

Kargo is **Cargo for Kotlin**: a build tool and package manager written in Rust. One `kargo.toml` and one CLI (`kargo new`, `build`, `run`, `test`, `add`, `tree`, `package`, ...) build Kotlin projects for the **JVM, WebAssembly, Kotlin/Native and Android**.

Design points that shape most changes:

- **No Gradle for JVM, Wasm and Native.** Kargo downloads `kotlin-compiler-embeddable` / `kotlin-native-prebuilt` from Maven Central and drives the compilers directly. It can also download a Temurin JDK (`[toolchain] jdk = "21"`).
- **Android is the exception.** Kargo generates a Gradle project under `target/android` from `kargo.toml` and drives the Android Gradle Plugin. Users never write Gradle files.
- **Real Maven resolution** (POM inheritance, BOMs, exclusions, version ranges, relocations, Gradle module metadata) with Gradle-style conflict resolution (highest version wins unless `force = true`).
- **`kargo.lock`** pins every resolved version and the SHA-256 of every artifact.
- **Cargo-shaped UX**: Cargo-style status output, exit code 101 on build/test failure, `kargo <task>` and `kargo-<name>` subcommand plugins.
- **Built-in frameworks** (`[frameworks]`: serialization, coroutines, compose, ktor, lombok, spring-boot) contribute libraries, compiler plugins and compiler flags per target.

Users get the full picture from `README.md` and the website in `website/`. Read them before changing behavior.

## Repository layout

Cargo workspace (edition 2024, MSRV 1.89). Crates depend downward in roughly this order:

| Path                     | Responsibility                                                                                                                                                |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/kargo-util`      | Terminal output (`Shell`), global `Config`, filesystem / hash / process helpers                                                                               |
| `crates/kargo-maven`     | Coordinates, Maven versions and ranges, POMs, Gradle module metadata, downloads, the resolver, uploads (`deploy.rs`) and the Maven Central Portal (`central.rs`) |
| `crates/kargo-manifest`  | The `kargo.toml` model (`raw.rs` is the serde layer, `lib.rs` the validated model), format-preserving edits (`edit.rs`)                                       |
| `crates/kargo-toolchain` | JDK detection and Temurin downloads, Kotlin compiler, Kotlin/Native, Gradle distributions, Kotlin language servers and the Kotlin debug adapter (`language_server.rs`), browsers, archives |
| `crates/kargo-compiler`  | JVM / Wasm / Native compiler drivers, the compiler daemon (`daemon.rs`, `daemon/KargoDaemon.java`), incremental compilation (`incremental.rs`, `daemon/KargoIc.java`), ABI fingerprints of path dependency jars (`abi.rs`), jar packaging, entry points, test reports (`testreport.rs`), compiler diagnostics (`diagnostics.rs`) |
| `crates/kargo-ops`       | The build engine: `workspace.rs`, `ops/` (one module per command), `builtins.rs` (frameworks), `lockfile.rs`, `gradle.rs` (Android), `tasks.rs`, `plugins.rs`, `warnings.rs` (warnings about valid but suspicious projects), `npm.rs` (npm installs) and `dts/` (Kotlin declarations generated from the TypeScript ones of npm packages) |
| `crates/kargo-lsp`       | `kargo lsp`: the message loop in front of a Kotlin language server (`server.rs`), Kargo's own Kotlin server, the default (`kotlin/`: lexer, declaration and scope parser `syntax.rs`, Java declarations `java.rs`, the JDK's sources `jdk.rs`, indexes `index.rs`, resolution and type inference `resolve.rs`, completion, hover, navigation and rename `features.rs`, standard library and sources jars `library.rs` with the offline outline `stubs.kt`, libraries without sources as generated stubs `compiled.rs`, read from class files `classfile.rs` and from Kotlin metadata and klibs `kmetadata.rs`, its message loop `server.rs`), `kargo.toml` diagnostics, completion and hover from the JSON Schema (`manifest.rs`, `schema.rs`), Maven coordinate and version completion and newer-version hints (`deps.rs`), code actions and the `kargo/addDependency` / `kargo/addTarget` commands (`actions.rs`), the `workspace.json` / `kls-classpath` project import and runnables (`project.rs`), check on save: `kargo check` diagnostics merged into the Kotlin server's (`check.rs`), `kargo dap`: kotlin-debug-adapter behind a proxy that launches `kargo run`/`test --debug` and maps the source layout (`dap.rs`), Zed's `.zed/tasks.json` for `kargo ide --zed` (`zed.rs`) |
| `src/`                   | The `kargo` binary: `cli.rs` (clap definitions and dispatch), `main.rs`                                                                                       |
| `tests/`                 | `cli.rs` offline CLI integration tests, `e2e.rs` real-compiler tests, `schema.rs` checks of the `kargo.toml` JSON Schema, `common/` shared helpers             |
| `editors/jetbrains`      | The IntelliJ IDEA / Android Studio plugin (Kotlin, Gradle, LSP4IJ): imports the project model from `kargo metadata` (modules, roots, libraries, Kotlin facets) and runs `kargo lsp`, plus the New Project wizard, run configurations and test tree, debugging, tool window and actions. Build with `./gradlew buildPlugin test` there |
| `editors/zed`            | The Zed extension (Rust, compiled to `wasm32-wasip2`, its own crate outside the workspace): runs `kargo lsp` and `kargo dap`, the `kargo` debug locator, the `/kargo-metadata` slash command. Build with `cargo test` and `cargo build --release --target wasm32-wasip2` there |
| `editors/vscode`         | The VS Code extension (TypeScript, compiled with `tsc`, `vscode-languageclient`): runs `kargo lsp` per project folder, the `kargo` tasks with their compiler errors in the Problems view, a test explorer over `kargo test --list` and `--message-format json`, Run and Debug code lenses, `kargo dap`, and a status bar for the target and profile. Build with `npm ci`, `npm test` and `npm run package` there |
| `website/`               | The documentation site (Astro + Starlight), deployed to GitHub Pages by `.github/workflows/website.yml`                                                       |

Where things live when you need to change them:

- **A command or flag**: `src/cli.rs` (clap), implemented in `crates/kargo-ops/src/ops/<command>.rs`.
- **A `kargo.toml` key**: `crates/kargo-manifest/src/raw.rs` (all structs use `deny_unknown_fields`, so a new key must be added there), then the validated model in `lib.rs`, then `edit.rs` if `kargo add`/`remove` must write it, and the JSON Schema in `website/public/schemas/kargo.json` that editors use (`tests/schema.rs` checks every key it lists against the parser, and its enums and default versions against Kargo's).
- **`kargo metadata`** (the project model for editors): `crates/kargo-ops/src/ops/metadata.rs`. Its output is versioned (`FORMAT_VERSION`): add fields freely, but removing or redefining one needs a new version. `kargo lsp` and the JetBrains plugin's project import (`editors/jetbrains/.../project/KargoMetadata.kt`) read it.
- **`kargo lsp`** (the language server): `crates/kargo-lsp`. It reads the schema at `website/public/schemas/kargo.json` (compiled in) and builds on `kargo metadata`, so a field added there can be passed on to the Kotlin servers in `project.rs`. Its code actions edit through `ManifestEditor` and the split-out halves of the commands (`ops::add::add_to`, `ops::target::edit` and `scaffold_targets`), so keep a command and its action on the same code path. The servers themselves are downloaded and pinned in `crates/kargo-toolchain/src/language_server.rs`.
- **The JetBrains plugin**: `editors/jetbrains` (Kotlin; `./gradlew buildPlugin test`, needs JDK 21). It starts `kargo lsp` with the options in `lsp/KargoLanguageServerFactory.kt`, imports the project from `kargo metadata` (`project/`: `KargoMetadata` reads the fields, `KargoImportPlanner` maps them to modules and libraries, `KargoProjectImporter` applies them, and `KotlinSupport` is the only code that touches the Kotlin plugin's internal facet and library-kind classes, so check `./gradlew verifyPlugin -PverifyIde=<newest IDE>` after changing it), and uses `kargo/runnables`, the `kargo/*` commands and `kargo test --message-format json`, so when you change one of those, check the plugin still reads it. Its guide is the IntelliJ IDEA and Android Studio section of `guides/editors.md`.
- **The Zed extension**: `editors/zed` (Rust; `cargo test` and `cargo build --release --target wasm32-wasip2` there). It runs `kargo lsp` (with `checkOnSave` on), `kargo dap` with the launch configuration in `debug_adapter_schemas/Kargo.json`, and `kargo metadata --no-deps`; its run buttons use the tasks `kargo ide --zed` writes (`crates/kargo-lsp/src/zed.rs`) with the tags of Zed's Kotlin extension. When you change those commands or `kargo dap`'s launch arguments, check the extension still matches. Its guide is the Zed section of `guides/editors.md`.
- **The VS Code extension**: `editors/vscode` (TypeScript). The pure modules (`commands.ts`, `diagnostics.ts`, `executable.ts`, `kotlinScan.ts`, `maven.ts`, `messages.ts`, `metadata.ts`, `process.ts`) have no `vscode` import, so `npm test` runs their unit tests in `src/test/`; `npm run test:integration` runs `src/integration/` in VS Code 1.100 on a new `kargo new` project, so run it after changing the extension's behavior. `servers.ts` starts `kargo lsp` per folder with a `kargo.toml`, `tasks.ts` runs the `kargo` tasks and reads their compiler messages, `testing.ts` is the test explorer (`kargo test --list --format json`, `--message-format json`), `codeLens.ts` the Run and Debug buttons, `debug.ts` runs `kargo dap`, and `actions.ts` the commands. Every command line is built in `commands.ts`, and `metadata.ts` reads `kargo metadata --no-deps`. When you change those commands, the JSON messages or `kargo dap`'s launch arguments, check the extension still matches. Its guide is the Visual Studio Code section of `guides/editors.md`.
- **Kargo's own Kotlin server** (`kargo lsp --server kargo`, the default; IntelliJ keeps `--server none`): `crates/kargo-lsp/src/kotlin`. It runs in-process behind `server.rs` like any other backend. It reads the project from `kargo metadata --no-deps` (`kotlin/server.rs`, `Model`: source roots per package, set and target), so a change to source layout or metadata must keep `model_from_metadata` right. Its tests run against the outline of the standard library in `stubs.kt`; `KOTLIN_SOURCES=<extracted stdlib sources> cargo test -p kargo-lsp -- --ignored` runs them against the real one. Its guide is `guides/kotlin-server.md`.
- **The debug adapter** (`kargo dap`): `crates/kargo-lsp/src/dap.rs`. kotlin-debug-adapter is pinned in `language_server.rs` (`KDA_VERSION`, `KDA_SHA256`).
- **Wasm output and browser support**: compiler flags in `crates/kargo-compiler/src/wasm.rs`; the page of a browser app, the import map for npm packages, a library's `package.json`, `wasm-opt` and the size report in `crates/kargo-ops/src/wasm_output.rs`; the loopback server behind `kargo run` and the browser test page in `crates/kargo-ops/src/web.rs`; browser lookup in `crates/kargo-toolchain/src/browser.rs`; npm installs in `crates/kargo-ops/src/npm.rs`; Kotlin declarations for npm packages in `crates/kargo-ops/src/dts/` (`program.rs` finds and reads the `.d.ts` files, `parser.rs` parses them, `kotlin.rs` writes the `external` declarations; bump `dts::FORMAT` when the output changes), generated by `npm::declarations` before each Wasm compilation and by `kargo lsp` on load.
- **A built-in framework**: `crates/kargo-ops/src/builtins.rs`; project templates for `kargo new` are in `crates/kargo-ops/src/ops/new.rs`.
- **Dependency resolution**: `crates/kargo-maven/src/resolver.rs`; locking in `crates/kargo-ops/src/lockfile.rs`.
- **Publishing** (`kargo publish`): the command in `crates/kargo-ops/src/ops/publish.rs`; the modules, POMs and `.module` files in `crates/kargo-ops/src/publication.rs`; GnuPG signing in `crates/kargo-ops/src/signing.rs`; uploads to repositories in `crates/kargo-maven/src/deploy.rs`; the Maven Central Portal in `crates/kargo-maven/src/central.rs`.
- **Source-layout conventions** (`src/`, `src-<target>/`, `tests-<target>/`, `resources/`, `android/`): `crates/kargo-ops/src/workspace.rs`.
- **A warning about a valid but suspicious project** (no sources, ignored `.kts`, a task hidden by a built-in command, an odd version): `crates/kargo-ops/src/warnings.rs`. Source checks run from `ops/build.rs` (`warn_about_sources`, also used by `check` and `package`; `test` reports only ignored files), manifest checks from `warn_manifest` in `src/cli.rs`. List each warning in `website/src/content/docs/reference/warnings.md`.
- **Compiler output** (relaying, `--message-format json` and its `target` field, the order of the `-Werror` summary): `crates/kargo-compiler/src/diagnostics.rs`; `kargo lsp` turns it into editor diagnostics in `crates/kargo-lsp/src/check.rs`.
- **Workspaces and path dependencies**: `Project` in `crates/kargo-ops/src/workspace.rs` loads members and path dependencies (a `Workspace` there is one package); `BuildContext::for_package` and `local_dependencies` in `ops/context.rs` share state between packages; `ops/local.rs` builds dependency jars and klibs.

## Build, test, format

```sh
cargo build
cargo test --workspace                       # unit + CLI integration tests; offline, no JDK needed
cargo fmt --all                              # always before finishing; rustfmt.toml sets max_width = 140
cargo clippy --workspace --all-targets       # don't introduce new warnings

KARGO_E2E=1 cargo test --test e2e            # real compilers: JVM, Wasm, Spring, Ktor, Compose, pinned JDK
KARGO_E2E=1 KARGO_E2E_NATIVE=1 KARGO_E2E_ANDROID=1 cargo test --test e2e   # also Native and Android (slow)
```

The default `cargo test` must never need network access or a JDK. The e2e tests are gated behind `KARGO_E2E`.

## Code conventions

Match the surrounding code. In particular:

- **Errors**: `anyhow::Result` with `.context(...)` / `bail!`. `thiserror` is used only in `kargo-maven`. Error messages are plain lowercase sentences that tell the user what to do next. `cli.rs` prints the cause chain and exits with 101.
- **Output**: user-facing status goes through `kargo_util::shell::Shell` (`status("Compiling", ...)`, `note`, `error`; stderr, honoring `-q`/`-v`). Only the program or test output of `kargo run` / `kargo test` goes to stdout. Don't `println!` from library crates.
- **Docs in code**: every module starts with a `//!` comment; public items get `///` where the name alone is not enough.
- **Cross-platform**: Kargo runs on Windows, macOS and Linux. Use `std::path`, the helpers in `kargo-util`, and avoid shell-specific assumptions. Files use LF line endings (`.gitattributes`).
- **Pinned versions** live in constants (`DEFAULT_KOTLIN_VERSION`, `DEFAULT_JVM_TARGET` in `kargo-manifest`; the `*_VERSION` constants in `builtins.rs` and `gradle.rs`; the tool versions `DOKKA_VERSION`, `KTLINT_VERSION` and `DETEKT_VERSION` in `ops/doc.rs`, `ops/fmt.rs` and `ops/lint.rs`; the language servers' `KOTLIN_LSP_VERSION`, `KLS_VERSION` and `KLS_SHA256`, and the debug adapter's `KDA_VERSION` and `KDA_SHA256`, in `kargo-toolchain`'s `language_server.rs`). Don't hard-code a version elsewhere in Rust code.
- **Unknown `kargo.toml` keys are errors**, by design. Keep it that way.

## Tests

- Unit tests sit next to the code in `#[cfg(test)] mod tests` (`kargo-manifest` keeps them in `src/tests.rs`).
- New CLI behavior gets a test in `tests/cli.rs`. Use `tests/common`: `Project` (scratch directory with an isolated `KARGO_HOME`) and `LocalRepo` (an on-disk `file://` Maven repository), so the test stays offline.
- Behavior that depends on a real Kotlin compiler gets an e2e test in `tests/e2e.rs`, behind `require_e2e!()`.
- Fix a bug by adding a test that fails without the fix.

## Documentation is part of the change

**Every change that affects what a user sees, types, configures or gets must update the documentation in the same change.** That means commands, flags, aliases and their output; `kargo.toml` keys and defaults; frameworks; environment variables; the lockfile; the project layout; toolchains; tasks and plugins; target behavior; error messages worth explaining; and default versions.

The docs are two things that must stay consistent:

1. **The website**, in `website/src/content/docs/` (Markdown, rendered by Starlight). This is the full documentation.
2. **`README.md`**, which mirrors the headline content: the command table, `kargo.toml` overview, project layout, environment table and the crate table. Update its matching section whenever you update the website page for the same thing.

Refactors, internal tests and performance work with no visible effect don't need docs. Say so in your summary rather than leaving it implicit.

### Which page to update

| If you change...                                                  | Update (under `website/src/content/docs/` unless noted)                                                                                                                                    |
| ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| A command, flag, alias or its output (`src/cli.rs`, `ops/`)       | `reference/commands.md`, the `README.md` command table, and the guide that uses it                                                                                                         |
| A `kargo.toml` key, default or validation rule                    | `reference/manifest.md`, the `README.md` `kargo.toml` section, the relevant guide, and the JSON Schema `website/public/schemas/kargo.json`                                                 |
| A built-in framework or its `kargo new --with` template           | `guides/frameworks.md`, the framework's own guide (`ktor.md`, `spring-boot.md`, `compose.md`), `reference/manifest.md`; keep the `--with` help text in `src/cli.rs` and the README in sync |
| An environment variable Kargo reads or sets                       | `reference/environment.md` and the `README.md` environment table                                                                                                                           |
| The lockfile format or resolution behavior                        | `reference/lockfile.md`, `guides/dependencies.md`                                                                                                                                          |
| JDK or Kotlin/Native toolchain handling                           | `guides/toolchains.md`, `guides/native.md`, the `README.md` toolchains section                                                                                                             |
| Tasks, hooks or plugins                                           | `guides/tasks-and-plugins.md`, `reference/environment.md` if hook variables change                                                                                                         |
| A target (Wasm, Native, Android, multiplatform)                   | `guides/wasm.md`, `guides/native.md`, `guides/android.md`, `guides/multiplatform.md`                                                                                                       |
| Test running or reporting                                         | `guides/testing.md`                                                                                                                                                                        |
| Source layout or generated project files                          | `getting-started/project-layout.md`, `getting-started/quick-start.md`, the `README.md` layout block                                                                                        |
| Install or build prerequisites                                    | `getting-started/installation.md`                                                                                                                                                          |
| Exit codes, `--locked`, `--offline` or other CI-relevant behavior | `guides/ci.md`                                                                                                                                                                             |
| Semantics people compare to Gradle, Maven or Cargo                | `migration/from-gradle.md`, `migration/from-maven.md`, `migration/from-cargo.md`                                                                                                           |
| The JetBrains plugin (`editors/jetbrains`)                         | The IntelliJ IDEA and Android Studio section of `guides/editors.md`, and the rest of that page where it mentions the plugin                                                                                                                  |
| Kargo's Kotlin server (`crates/kargo-lsp/src/kotlin`)             | `guides/kotlin-server.md`, and the parts of `guides/editors.md` that describe what `kargo lsp` gives Kotlin files                                                                                                  |
| The Zed extension (`editors/zed`), `kargo dap` or `kargo ide --zed` | The Zed section of `guides/editors.md`, and the rest of that page where it mentions them                                                                                                                              |
| The VS Code extension (`editors/vscode`) | The Visual Studio Code section of `guides/editors.md`, and the rest of that page where it mentions the extension (its test explorer and debugging too)                                                          |
| A common failure or workaround                                    | `reference/faq.md`                                                                                                                                                                         |
| Crates added, removed or re-scoped; build or test workflow        | `reference/contributing.md`, the `README.md` Development section, and this file                                                                                                            |
| A default version (Kotlin, JVM target, framework versions)        | Search `website/src`, `website/public/schemas/kargo.json` and `README.md` for the old value and update every occurrence                                                                   |

When unsure, `grep -rn` the old command, flag, key or value across `website/src/content` and `README.md` and fix every hit.

### When no page fits: add one

If the change introduces something the existing pages don't cover (a new framework, a new target, a new workflow), create a new page rather than stretching an unrelated one.

1. **Pick the section.** `guides/` for task-oriented how-tos, `reference/` for exhaustive lookups, `getting-started/` for onboarding, `migration/` for comparisons with other tools.
2. **Create** `website/src/content/docs/<section>/<slug>.md` with this frontmatter (both fields are required):
   ```md
   ---
   title: Short page title
   description: One sentence on what the reader will learn or find.
   ---
   ```
3. **Add it to the sidebar** in `website/astro.config.mjs`, in the matching group: `{ label: 'Short title', slug: '<section>/<slug>' }`. The slug has no extension and no leading slash. Pages not in the sidebar are effectively hidden.
4. **Link it** from the related pages (and from `reference/commands.md` or `reference/manifest.md` when it documents a command or key).
5. Add a card or link on `index.mdx` only if it is a headline feature.

### Writing style

Follow the existing pages:

- Second person, present tense, short paragraphs. Show a working example first, then explain the options.
- Code fences: `sh` for commands to run; `console` with `$ ` prompts when showing a command and its output; `toml title="kargo.toml"`, `kotlin title="src/Main.kt"` and similar for files. Use tables for key and flag references.
- Asides: `:::note`, `:::tip`, `:::caution` (Starlight syntax).
- Internal links are relative with a trailing slash, e.g. `[Frameworks](../frameworks/)` or `[Commands](../../reference/commands/)`. The site is served under a base path on GitHub Pages, so never use root-absolute links like `/guides/...`.
- Every command, flag, key and output in the docs must be real. Check with `kargo <command> --help` and by running the example instead of writing it from memory.

### Verify the site builds

```sh
cd website
bun install          # first time only
bun run build        # must succeed; output goes to website/dist
bun run dev          # optional: preview at http://localhost:4321/
```

Fix any build error (bad frontmatter, a sidebar slug that doesn't match a file) before finishing. `website/dist`, `.astro` and `node_modules` are gitignored.

## Finishing a task

Before reporting a task as done:

1. `cargo fmt --all` and `cargo test --workspace` pass (run the e2e tests too when you touched compiler drivers, frameworks or toolchains).
2. The docs are updated per the section above, and `bun run build` in `website/` passes if you touched `website/`.
3. Your summary lists which docs pages you changed or created, or states why none were needed.
