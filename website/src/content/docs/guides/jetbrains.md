---
title: IntelliJ IDEA and Android Studio
description: Install the Kargo plugin for JetBrains IDEs to create, open, run, test and debug Kargo projects in IntelliJ IDEA and Android Studio.
---

The Kargo plugin makes IntelliJ IDEA and Android Studio understand Kargo projects. It [imports the project](#project-import) (modules, source roots and the libraries of `kargo.toml`) so the IDE's own Kotlin support resolves your dependencies, runs [`kargo lsp`](../editors/#language-server) inside the IDE for `kargo.toml` support, and adds what an IDE needs on top: a New Project wizard, run configurations, Run and Debug icons, a test tree and a Kargo tool window.

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

The project opens with `src/Main.kt`, and the plugin starts [importing it](#project-import).

For a folder you already opened, *Tools → Kargo → Initialize Kargo Project* runs `kargo init` in its root.

## Project import

An IDE can only complete and resolve what it knows about. When a folder with a `kargo.toml` opens, the plugin runs [`kargo metadata`](../../reference/machine-readable-output/#kargo-metadata) in the background (the *Importing Kargo project* progress), which resolves the dependencies as `kargo build` would, and describes the result to the IDE:

| From Kargo | In the IDE |
|---|---|
| A package | A module named after it |
| `src/`, `src-<target>/` | Source roots |
| `tests/`, `tests-<target>/` | Test source roots |
| `resources/`, `test-resources/` | Resource roots |
| `target/` | Excluded from the project |
| A Maven dependency | A project library named `Kargo: <group>:<artifact>:<version>`, with its sources jar if `kargo fetch --sources` downloaded it |
| `[dev-dependencies]` and runtime-only dependencies | The same libraries with *Test* and *Runtime* scope |
| A path dependency or workspace member | A module dependency on that package's module |
| `[package] kotlin`, `language-version`, `api-version`, `opt-in`, `compiler-args` and compiler plugins | The module's Kotlin facet |
| `[toolchain] jdk` | The module's SDK, added to the IDE's JDK table |
| The target | The module's Kotlin platform: JVM, Kotlin/Native or Wasm |

Reopen a JVM project and its Maven libraries resolve: completion, go to definition (into the sources jar, once fetched), quick documentation and error highlighting all know the libraries of `kargo.toml`.

**Kotlin/Native.** A Native module is analyzed as Kotlin/Native. Its libraries are the klibs of `[target.native.dependencies]`, plus the standard library and the platform libraries (`posix`, `windows`, ...) of the Kotlin/Native distribution Kargo downloaded. A library that is published only as Native klibs, such as a binding to a C library, resolves like any other. When the target builds for several Native platforms, the IDE analyzes the first.

**Several targets.** A package with more than one target gets a module for `src/` and `tests/` (`<name>.common`) and one per target (`<name>.jvm`, `<name>.native`, `<name>.wasm`) for `src-<target>/`, which refines the common one, so `expect` and `actual` declarations find each other. Common code is analyzed like the package's first target (JVM, then Native, then Wasm), with that target's libraries; `kargo check` still compiles it for every target. Android code isn't imported: Gradle owns it, see [Android](#android).

The import runs again, without any action, when a `kargo.toml` or `kargo.lock` changes (including when `kargo add`, `kargo update` or a build rewrites them) and when a `src*`, `tests*` or `resources` directory appears or goes away. *Tools → Kargo → Fetch Dependencies and Sources* imports once more to pick up the sources jars. To import by hand, use *Tools → Kargo → Reload Kargo Project*. If `kargo.toml` and `kargo.lock` are as they were at the last import, the check takes a moment and the IDE doesn't index anything.

The modules are saved in the IDE's `.idea/` directory (which `kargo new` git-ignores), so a project opens with its last known model, even offline. The import only manages what it created: the modules it recorded in `.idea/kargo.xml` and the libraries whose name starts with `Kargo: `. A module of your own with the same name as a package is taken over when it only covers that package, and left alone otherwise.

:::caution
The IDE's Kotlin support reads libraries compiled by the Kotlin version it ships with, or one newer at most. A project on a newer Kotlin than the IDE understands shows its libraries, and even `String` and `println`, as unresolved. That is the case for Kargo's default Kotlin and IntelliJ IDEA 2025.2 (Kotlin 2.2): a Native project can't read the standard library, and a JVM project the libraries compiled with Kotlin 2.4. Use an IDE that bundles a Kotlin plugin at least as new as `[package] kotlin` (IntelliJ IDEA 2026.2 has Kotlin 2.4), or set an older `kotlin` in `kargo.toml`. *Settings → Plugins* shows the IDE's Kotlin version.
:::

If resolving the dependencies fails (offline with an empty cache, a library that doesn't exist), the plugin still imports the modules and source roots and shows a *Kargo dependencies are not resolved* balloon with Kargo's message. Fix the cause, then reload.

## Code intelligence

When you open `kargo.toml`, the plugin starts `kargo lsp --server none` in the project root, which is `kargo lsp` without a Kotlin language server. It gives you the `kargo.toml` half of [Editor integration](../editors/#language-server): completion, documentation, validation and quick fixes, with Maven coordinate and version completion and hints at newer versions. It also answers [`kargo/runnables`](../editors/#runnables) for the tool window and the commands behind *Add Dependency* and *Add Target*.

Kotlin files are not sent to `kargo lsp`: the IDE's own Kotlin support analyzes them against the [imported project](#project-import). Errors from `kargo check` are not shown in the editor either, which matters for what the IDE can't analyze, like code that fails on only one target; run *Tools → Kargo → Check*. LSP4IJ shows the server's log in *View → Tool Windows → Language Servers*.

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
| Fetch Dependencies and Sources | `kargo fetch --sources`, for navigation into library sources; the [project import](#project-import) then picks them up |
| Update Dependencies | `kargo update` |
| Reload Kargo Project | [Imports the project](#project-import) again and restarts `kargo lsp` |
| Initialize Kargo Project | `kargo init`, in a project without `kargo.toml` |

*Add Dependency* and *Add Target* go through `kargo lsp`'s [commands](../editors/#commands-for-editor-plugins), so they edit `kargo.toml` the same way the quick fixes do. You can assign any of these actions a shortcut in *Settings → Keymap*.

## Settings

*Settings → Languages & Frameworks → Kargo* applies to every project. Changing it restarts `kargo lsp` and imports the project again in open Kargo projects.

| Setting | Default | Meaning |
|---|---|---|
| Kargo executable | empty | The `kargo` to run; empty: the one on `PATH`, then `~/.cargo/bin` |
| Import the project model from Kargo | on | Describe the modules, source roots and libraries to the IDE ([Project import](#project-import)). Turn it off if you manage the project model yourself; modules already imported stay |
| Run kargo lsp | on | Run the language server for `kargo.toml` |

## Android

Android Studio runs the plugin like IntelliJ IDEA does: `kargo.toml` support, the project import, run configurations and the tool window work for every target but Android. Android sources (`src-android/`, `android/`) aren't imported, because Kargo hands Android to Gradle and the Android Gradle Plugin. Layout and Compose previews need Gradle's model. For those, open the Gradle project Kargo generates in `target/android` (see [Android](../android/)) in a second window.

## Limitations

- *Build → Build Project* doesn't run `kargo build`; use *Tools → Kargo → Build*, or check on save for errors in the editor.
- *Rerun Failed Tests* is not available; rerun a single test from its gutter icon.
- The IDE analyzes each module for one platform. In a package with several targets, common code (`src/`) is analyzed like the first target, so completion there also offers that platform's APIs. *Tools → Kargo → Check* shows what fails on the other targets.
- Wasm modules are analyzed against the Wasm klibs of `kargo.toml`, but browser APIs and npm packages are not described to the IDE.
- The IDE reads libraries compiled by its own Kotlin version or one newer at most, see the note under [Project import](#project-import).
