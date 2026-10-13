---
title: IntelliJ IDEA and Android Studio
description: Install the Kargo plugin for JetBrains IDEs to create, open, run, test and debug Kargo projects in IntelliJ IDEA and Android Studio.
---

The Kargo plugin makes IntelliJ IDEA and Android Studio understand Kargo projects. It runs [`kargo lsp`](../editors/#language-server) inside the IDE, so you get the same Kotlin code intelligence, `kargo.toml` support and check on save as other editors, and adds what an IDE needs on top: a New Project wizard, run configurations, Run and Debug icons, a test tree and a Kargo tool window.

Open a folder that contains a `kargo.toml` and the plugin takes over. You don't need a `build.gradle.kts` or a `pom.xml`.

## Install

The plugin needs:

- IntelliJ IDEA 2025.2 or newer, or Android Studio based on it (Otter or newer).
- [LSP4IJ](https://plugins.jetbrains.com/plugin/23257-lsp4ij), Red Hat's LSP client for JetBrains IDEs. Install it from *Settings → Plugins → Marketplace* first.
- Kargo itself. See [Installation](../../getting-started/installation/).

The plugin isn't on the JetBrains Marketplace yet. Build it from the repository and install the zip:

```sh
cd editors/jetbrains
./gradlew buildPlugin
```

Then, in the IDE, open *Settings → Plugins → ⚙ → Install Plugin from Disk…* and pick `editors/jetbrains/build/distributions/kargo-jetbrains-<version>.zip`. Every build on `master` also attaches the zip as an artifact of the *JetBrains plugin* workflow on GitHub Actions.

The plugin looks for `kargo` on your `PATH`, then in `~/.cargo/bin`. If Kargo is somewhere else, set *Kargo executable* in *Settings → Languages & Frameworks → Kargo*. If it can't find Kargo when a Kargo project opens, it says so and links to the installation guide.

## Create a project

*File → New → Project → Kargo* runs [`kargo init`](../../reference/commands/) in the new project's directory with the options you pick:

| Field | `kargo init` option |
|---|---|
| Name, Location | The directory, and the package name |
| Library | `--lib` |
| Targets | `--target jvm,wasm,native,android` |
| Frameworks | `--with compose,ktor,...` (see [Built-in frameworks](../frameworks/)) |
| Group | `--group` |
| Kotlin version | `--kotlin` (empty: the version Kargo pins) |
| Create Git repository | `--no-vcs` when unchecked |

The project opens with `src/Main.kt`, and `kargo lsp` starts importing it.

For a folder you already opened, *Tools → Kargo → Initialize Kargo Project* runs `kargo init` in its root.

## Code intelligence

When you open a Kotlin file or `kargo.toml`, the plugin starts `kargo lsp` in the project root. It gives you everything listed in [Editor integration](../editors/#language-server):

- Completion, go to definition (into library sources once you run *Tools → Kargo → Fetch Dependencies and Sources*), find usages, hover, rename and quick fixes in Kotlin files, from JetBrains' Kotlin LSP. Kargo describes your project to it from `kargo.toml`, and describes it again when `kargo.toml` or `kargo.lock` change.
- Completion, documentation, validation and quick fixes in `kargo.toml`, with Maven coordinate and version completion and hints at newer versions.
- [Check on save](../editors/#check-on-save): when you save a Kotlin file, the errors of `kargo check` for every target appear in the editor and the *Problems* view. The plugin turns it on by default.

The first start downloads the Kotlin LSP and resolves your dependencies, so it takes a while. LSP4IJ shows its progress in the status bar, and the server's log in *View → Tool Windows → Language Servers*.

The plugin uses Kargo for Kotlin files instead of the IDE's own Kotlin support, which needs a Gradle or Maven project to know your dependencies.

## Run, test and debug

A green arrow in the gutter runs:

| Next to | Runs |
|---|---|
| A top-level `fun main` | The program: `kargo run -t <target>`, `--bin <name>` for a [`[[bin]]`](../multiple-programs/), `--example <name>` in `examples/` |
| A function annotated with `@Test` (or `@ParameterizedTest`, `@RepeatedTest`, `@TestFactory`) | That test: `kargo test <package.Class.method>` |
| A test class | Its tests: `kargo test <package.Class>` |

The target comes from the directory: `src-wasm/` and `tests-wasm/` run on Wasm, and so on. Tests in `tests/` run on every enabled target.

Each click creates a temporary *Kargo* run configuration. You can also add one yourself in *Run → Edit Configurations → + → Kargo*:

| Field | Meaning |
|---|---|
| Command | What follows `kargo`, such as `run -t jvm`, `test MathTest`, `build --release` or `task lint` |
| Working directory | The package to run in; empty: the project root |
| Environment variables | Added to the environment of `kargo` |

Compiler errors and file locations in the output are links to the source.

**Tests.** A configuration whose command is `kargo test` shows its results in the IDE's test tree: one node per test class and test, with its output, failure message, stack trace and duration. The plugin adds `--message-format json` for this; see [Test results](../../reference/machine-readable-output/#test-results).

**Debugging.** *Debug* on a `run` or `test` configuration adds [`--debug=<port>`](../editors/#debugging) with a free port, and `-t jvm` unless the command names a target. The IDE's Java debugger attaches as soon as the JVM listens, so breakpoints, stepping and variables work as in any Java program. Debugging works for the JVM target only.

## The Kargo tool window

*View → Tool Windows → Kargo* lists what the project can run, from [`kargo/runnables`](../editors/#runnables): each target's program, `[[bin]]` and example, each target's tests, and the [tasks](../tasks-and-plugins/) in `kargo.toml`. Double-click an entry to run it, or select it and use *Debug* in the toolbar. *Refresh* lists them again after you change `kargo.toml`.

## Kargo actions

*Tools → Kargo* runs Kargo's commands for the package of the file you're in:

| Action | Runs |
|---|---|
| Build, Check, Test, Clean | `kargo build`, `kargo check`, `kargo test`, `kargo clean` |
| Run Command… | Any `kargo` command line you type |
| Run Task… | `kargo task <name>`, picked from the tasks in `kargo.toml` |
| Add Dependency… | `kargo add` for coordinates you type, into `[dependencies]` or `[dev-dependencies]` |
| Remove Dependency… | `kargo remove <name>` |
| Add Target… | `kargo target add <target>` |
| Fetch Dependencies and Sources | `kargo fetch --sources`, for navigation into library sources |
| Update Dependencies | `kargo update` |
| Reload Kargo Project | Restarts `kargo lsp`, which imports the project again |
| Initialize Kargo Project | `kargo init`, in a project without `kargo.toml` |

*Add Dependency* and *Add Target* go through `kargo lsp`'s [commands](../editors/#commands-for-editor-plugins), so they edit `kargo.toml` the same way the quick fixes do. You can assign any of these actions a shortcut in *Settings → Keymap*.

## Settings

*Settings → Languages & Frameworks → Kargo* applies to every project. Changing it restarts `kargo lsp` in open Kargo projects.

| Setting | Default | Meaning |
|---|---|---|
| Kargo executable | empty | The `kargo` to run; empty: the one on `PATH`, then `~/.cargo/bin` |
| Run kargo lsp | on | Run the language server for Kotlin files and `kargo.toml` |
| Kotlin server | `kotlin-lsp` | [`kargo lsp --server`](../editors/#choosing-the-kotlin-server): `kotlin-lsp`, `kls`, or `none` for `kargo.toml` support only |
| Describe Wasm and Native sources | off | `kargo lsp --klib-targets`. See [Multiplatform projects](../editors/#multiplatform-projects) |
| Check on save | on | The `checkOnSave` option of [check on save](../editors/#check-on-save) |
| Targets to check | empty | `checkTargets`, comma-separated, such as `wasm,native`; empty: every enabled target |

## Android

Android Studio runs the plugin like IntelliJ IDEA does: Kotlin code intelligence, `kargo.toml` support, run configurations and the tool window work for every target. Code in `android/` gets no Kotlin code intelligence from the Kotlin LSP yet, and layout and Compose previews need Gradle's model. For those, open the Gradle project Kargo generates in `target/android` (see [Android](../android/)) in a second window.

## Limitations

- *Build → Build Project* doesn't run `kargo build`; use *Tools → Kargo → Build*, or check on save for errors in the editor.
- *Rerun Failed Tests* is not available; rerun a single test from its gutter icon.
- Wasm and Native code gets Kotlin code intelligence only with *Describe Wasm and Native sources*, and its libraries don't resolve yet. Check on save still reports its errors.
