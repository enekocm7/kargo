---
title: Editor integration
description: Pick an editor for Kargo projects. IntelliJ IDEA and Android Studio with the Kargo plugin are the best supported; VS Code and Zed have Kargo extensions, Neovim and Emacs have Kargo plugins, and other editors run kargo lsp, kargo dap and kargo ide.
---

Kargo gives editors what they need to understand a project without reading `kargo.toml` themselves: a language server, a JSON Schema for `kargo.toml`, a JSON description of the project, library sources, machine-readable compiler errors, test discovery and per-test results, and a debugger port.

**Use IntelliJ IDEA or Android Studio with the Kargo plugin if you can.** It is the best supported way to work on a Kargo project. Kotlin is developed by JetBrains, and IntelliJ IDEA has the most complete support for the language: its own analysis, refactorings, debugger and test tree. The plugin imports your project into that support, so your dependencies resolve and Run, Test and Debug work from the IDE. Zed and the other editors run `kargo lsp` instead, with [Kargo's own Kotlin server](../kotlin-server/): completion, navigation and rename for every target, but not the compiler's full analysis. The Kargo extension for VS Code adds tasks, a test explorer and Run and Debug buttons, and the other editors have no test tree.

## Which editor to use

| Editor | Set up with | Kotlin code | Run, test and debug |
|---|---|---|---|
| IntelliJ IDEA, Android Studio | The [Kargo plugin](#intellij-idea-and-android-studio) | The IDE's own Kotlin support, with your dependencies imported. JVM, Kotlin/Native and Wasm modules | Run configurations, Run and Debug icons, a test tree and the JVM debugger |
| VS Code | The [Kargo extension](#visual-studio-code) | `kargo lsp` with [Kargo's Kotlin server](../kotlin-server/) | Tasks with compiler errors in the Problems view, a test explorer, Run and Debug buttons, and debugging through `kargo dap` |
| Zed | The [Kargo extension](#zed) | `kargo lsp` with [Kargo's Kotlin server](../kotlin-server/) | Run buttons from `kargo ide --zed`, debugging through `kargo dap`, and test results in the terminal |
| Neovim | The [Neovim plugin](#neovim) in `editors/neovim` | `kargo lsp` with [Kargo's Kotlin server](../kotlin-server/) | `:Kargo` commands with errors in the quickfix list, runnables, a neotest adapter for tests, and debugging with nvim-dap |
| Helix | `kargo lsp` in the editor's configuration | `kargo lsp` with [Kargo's Kotlin server](../kotlin-server/) | Debugging through `kargo dap` with a DAP client, and tests in the terminal |
| Emacs | The [Emacs package](#emacs) in `editors/emacs` | `kargo lsp` with [Kargo's Kotlin server](../kotlin-server/), through Eglot or lsp-mode | Commands through `compile` with errors you can jump to, test and class runs with a results buffer, build errors in Flymake, and debugging with Dape |
| Other LSP clients | `kargo lsp` | `kargo lsp` with [Kargo's Kotlin server](../kotlin-server/) | Programs and tests in the terminal, or a JDWP debugger attached to `--debug` |

Every editor gets `kargo.toml` support, [runnables](#runnables) and [check on save](#check-on-save). Kargo's Kotlin server reads the code of every target, `src-wasm/`, `src-js/` and `src-native/` included, with each target's standard library, the JDK's sources, your `.java` files and your dependencies: from their sources jars once you fetch them, else from their compiled classes and klibs (see [Libraries](../kotlin-server/#libraries)). The npm packages of a Wasm target come from the [Kotlin declarations](../wasm/#kotlin-declarations-for-npm-packages) Kargo generates for them. Android code is handled by Android Studio, through the Gradle project Kargo generates in `target/android` (see [Android](../android/)).

## IntelliJ IDEA and Android Studio

The Kargo plugin makes IntelliJ IDEA and Android Studio understand Kargo projects. It [imports the project](#project-import) (modules, source roots and the libraries of `kargo.toml`) so the IDE's own Kotlin support resolves your dependencies, runs [`kargo lsp`](#language-server) inside the IDE for `kargo.toml` support, and adds what an IDE needs on top: a New Project wizard, run configurations, Run and Debug icons, a test tree and a Kargo tool window.

Open a folder that contains a `kargo.toml` and the plugin takes over. You don't need a `build.gradle.kts` or a `pom.xml`.

### Install the plugin

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

The plugin looks for `kargo` on your `PATH`, then in `~/.kargo/bin` (where the [install script](../../getting-started/installation/) puts it), then in `~/.cargo/bin`. If Kargo is somewhere else, set *Kargo executable* in *Settings → Languages & Frameworks → Kargo*. If it can't find Kargo when a Kargo project opens, it says so and links to the installation guide.

### Create a project

*File → New → Project → Kargo* runs [`kargo init`](../../reference/commands/) in the new project's directory with the options you pick:

| Field | `kargo init` option |
|---|---|
| Name, Location | The directory, and the package name |
| Library | `--lib` |
| Targets | `--target jvm,wasm,js,native,android` |
| Frameworks | `--with compose,ktor,...` (see [Built-in frameworks](../frameworks/)) |
| Group | `--group` |
| Kotlin version | `--kotlin` (empty: the version Kargo pins) |
| Create Git repository | `--no-vcs` when unchecked |

The project opens with `src/Main.kt`, and the plugin starts [importing it](#project-import).

For a folder you already opened, *Tools → Kargo → Initialize Kargo Project* runs `kargo init` in its root.

### Project import

An IDE can only complete and resolve what it knows about. When a folder with a `kargo.toml` opens, the plugin runs [`kargo metadata`](../../reference/machine-readable-output/#kargo-metadata) in the background (the *Importing Kargo project* progress), which resolves the dependencies as `kargo build` would, and describes the result to the IDE:

| From Kargo | In the IDE |
|---|---|
| A package | A module named after it |
| `src/`, `src-<target>/` | Source roots |
| `tests/`, `tests-<target>/` | Test source roots |
| `resources/`, `test-resources/` | Resource roots |
| `examples/` | A test source root, since examples are compiled against the package's sources |
| `target/` | Excluded from the project, except the output directories of [`[[generate]]` steps](../code-generation/) in `target/generated/`, which are source (or test source, or resource) roots of their own |
| `target/generated/<profile>/<name>/` of a [`[[generate]]` step](../code-generation/) | A source root, so generated code, such as classes from `.proto` files, resolves. The directory is filled by `kargo build`, `kargo check` or `kargo generate`; run one before the IDE needs the code |
| A Maven dependency | A project library named `Kargo: <group>:<artifact>:<version>`, with its sources jar if `kargo fetch --sources` downloaded it |
| An npm package of a Wasm target (from `[target.wasm.npm-dependencies]`, or one a Wasm library needs) | A project library named `Kargo: npm <name>@<version>`, with the package's directory in `target/<profile>/node_modules`. It is a runtime dependency of the Wasm module, or a test one for a package only the tests need. A package that isn't installed yet is left out until a build or `kargo fetch` installs it |
| `[dev-dependencies]` and runtime-only dependencies | The same libraries with *Test* and *Runtime* scope |
| A path dependency or workspace member | A module dependency on that package's module |
| `[package] kotlin`, `language-version`, `api-version`, `opt-in`, `compiler-args` and compiler plugins | The module's Kotlin facet |
| `[toolchain] jdk` | The module's SDK, added to the IDE's JDK table |
| The target | The module's Kotlin platform: JVM, Kotlin/Native or Wasm |

Reopen a JVM project and its Maven libraries resolve: completion, go to definition (into the sources jar, once fetched), quick documentation and error highlighting all know the libraries of `kargo.toml`.

**Kotlin/Native.** A Native module is analyzed as Kotlin/Native. Its libraries are the klibs of `[target.native.dependencies]`, plus the standard library and the platform libraries (`posix`, `windows`, ...) of the Kotlin/Native distribution Kargo downloaded. A library that is published only as Native klibs, such as a binding to a C library, resolves like any other. When the target builds for several Native platforms, the IDE analyzes the first.

**Several targets.** A package with more than one target gets a module for `src/` and `tests/` (`<name>.common`) and one per target (`<name>.jvm`, `<name>.native`, `<name>.wasm`) for `src-<target>/`, which refines the common one, so `expect` and `actual` declarations find each other. Common code is analyzed like the package's first target (JVM, then Native, then Wasm), with that target's libraries; `kargo check` still compiles it for every target. Android code isn't imported: Gradle owns it, see [Android](#android). Kotlin/JS code (`src-js/`) isn't imported yet either; open it with `kargo lsp` in another editor for now.

The import runs again, without any action, when a `kargo.toml` or `kargo.lock` changes (including when `kargo add` or `kargo update` rewrites them), when a `src*`, `tests*` or `resources` directory appears or goes away, and after *Build*, *Run*, *Test* or a fetch, which can install npm packages without changing either file. *Tools → Kargo → Fetch Dependencies and Sources* imports once more to pick up the sources jars. To import by hand, use *Tools → Kargo → Reload Kargo Project*. If `kargo.toml` and `kargo.lock` are as they were at the last import, the check takes a moment and the IDE doesn't index anything.

The modules are saved in the IDE's `.idea/` directory (which `kargo new` git-ignores), so a project opens with its last known model, even offline. The import only manages what it created: the modules it recorded in `.idea/kargo.xml` and the libraries whose name starts with `Kargo: `. A module of your own with the same name as a package is taken over when it only covers that package, and left alone otherwise.

:::caution
The IDE's Kotlin support reads libraries compiled by the Kotlin version it ships with, or one newer at most. A project on a newer Kotlin than the IDE understands shows its libraries, and even `String` and `println`, as unresolved. That is the case for Kargo's default Kotlin and IntelliJ IDEA 2025.2 (Kotlin 2.2): a Native project can't read the standard library, and a JVM project the libraries compiled with Kotlin 2.4. Use an IDE that bundles a Kotlin plugin at least as new as `[package] kotlin` (IntelliJ IDEA 2026.2 has Kotlin 2.4), or set an older `kotlin` in `kargo.toml`. *Settings → Plugins* shows the IDE's Kotlin version.
:::

If resolving the dependencies fails (offline with an empty cache, a library that doesn't exist), the plugin still imports the modules and source roots and shows a *Kargo dependencies are not resolved* balloon with Kargo's message. Fix the cause, then reload.

### Code intelligence in the IDE

When you open `kargo.toml`, the plugin starts `kargo lsp --server none` in the project root, which is `kargo lsp` without a Kotlin language server. It gives you the `kargo.toml` half of [Editor integration](#language-server): completion, documentation, validation and quick fixes, with Maven coordinate and version completion and hints at newer versions. It also answers [`kargo/runnables`](#runnables) for the tool window and the commands behind *Add Dependency* and *Add Target*.

Kotlin files are not sent to `kargo lsp`: the IDE's own Kotlin support analyzes them against the [imported project](#project-import). Errors from `kargo check` are not shown in the editor either, which matters for what the IDE can't analyze, like code that fails on only one target; run *Tools → Kargo → Check*. LSP4IJ shows the server's log in *View → Tool Windows → Language Servers*.

### Run, test and debug

A green arrow in the gutter runs:

| Next to | Runs |
|---|---|
| A top-level `fun main` | The program: `kargo run -t <target>`, `--bin <name>` for a [`[[bin]]`](../multiple-programs/), `--example <name>` in `examples/` |
| A function annotated with `@Test` (or `@ParameterizedTest`, `@RepeatedTest`, `@TestFactory`) | That test: `kargo test <package.Class.method>` |
| A test class | Its tests: `kargo test <package.Class>` |
| A top-level function annotated with `@Preview` | A render icon instead: [`kargo preview <package.function>`](../compose/#previews), which writes the PNG to `target/preview/` |

The target comes from the directory: `src-wasm/` and `tests-wasm/` run on Wasm, and so on. Tests in `tests/` run on every enabled target.

Each click creates a temporary *Kargo* run configuration. You can also add one yourself in *Run → Edit Configurations → + → Kargo*:

| Field | Meaning |
|---|---|
| Command | What follows `kargo`, such as `run -t jvm`, `test MathTest`, `build --release` or `task lint` |
| Working directory | The package to run in; empty: the project root |
| Environment variables | Added to the environment of `kargo` |

Compiler errors and file locations in the output are links to the source.

**Tests.** A configuration whose command is `kargo test` shows its results in the IDE's test tree: one node per test class and test, with its output, failure message, stack trace and duration. The plugin adds `--message-format json` for this; see [Test results](../../reference/machine-readable-output/#test-results).

**Debugging.** *Debug* on a `run` or `test` configuration adds [`--debug=<port>`](#debugging) with a free port, and `-t jvm` unless the command names a target. The IDE's Java debugger attaches as soon as the JVM listens, so breakpoints, stepping and variables work as in any Java program. Debugging works for the JVM target only.

### The Kargo tool window

*View → Tool Windows → Kargo* lists what the project can run, from [`kargo/runnables`](#runnables): each target's program, `[[bin]]` and example, each target's tests, the [Compose previews](../compose/#previews), and the [tasks](../tasks-and-plugins/) in `kargo.toml`. Double-click an entry to run it, or select it and use *Debug* in the toolbar. *Refresh* lists them again after you change `kargo.toml`.

### Kargo actions

*Tools → Kargo* runs Kargo's commands for the package of the file you're in:

| Action | Runs |
|---|---|
| Build, Check, Test, Clean | `kargo build`, `kargo check`, `kargo test`, `kargo clean` |
| Run Command… | Any `kargo` command line you type |
| Run Task… | `kargo task <name>`, picked from the tasks in `kargo.toml` |
| Add Dependency… | `kargo add` for coordinates you type, into `[dependencies]` or `[dev-dependencies]` |
| Remove Dependency… | `kargo remove <name>` |
| Add Target… | `kargo target add <target>` |
| Remove Target… | `kargo target remove <target>`, for the targets `kargo target list` shows as enabled |
| Fetch Dependencies and Sources | `kargo fetch --sources`, for navigation into library sources; the [project import](#project-import) then picks them up |
| Update Dependencies | `kargo update` |
| Reload Kargo Project | [Imports the project](#project-import) again and restarts `kargo lsp` |
| Initialize Kargo Project | `kargo init`, in a project without `kargo.toml` |

*Add Dependency* and *Add Target* go through `kargo lsp`'s [commands](#commands-for-editor-plugins), so they edit `kargo.toml` the same way the quick fixes do. You can assign any of these actions a shortcut in *Settings → Keymap*.

*Build → Build Project* runs `kargo build` for the modules the plugin imported, instead of the IDE's compiler. Its compiler errors and warnings show in the Build tool window and the Problems view, each linked to its line in the source.

### Plugin settings

*Settings → Languages & Frameworks → Kargo* applies to every project. Changing it restarts `kargo lsp` and imports the project again in open Kargo projects.

| Setting | Default | Meaning |
|---|---|---|
| Kargo executable | empty | The `kargo` to run; empty: the one on `PATH`, then `~/.kargo/bin`, then `~/.cargo/bin` |
| Import the project model from Kargo | on | Describe the modules, source roots and libraries to the IDE ([Project import](#project-import)). Turn it off if you manage the project model yourself; modules already imported stay |
| Run kargo lsp | on | Run the language server for `kargo.toml` |

### Android

Android Studio runs the plugin like IntelliJ IDEA does: `kargo.toml` support, the project import, run configurations and the tool window work for every target but Android. Android sources (`src-android/`, `android/`) aren't imported, because Kargo hands Android to Gradle and the Android Gradle Plugin. Run configurations of an Android app run `kargo run -t android`, so they follow [Devices and emulators](../android/#devices-and-emulators); where there is no terminal to ask in, set `KARGO_ANDROID_DEVICE` in the run configuration's environment. Layout and Compose previews need Gradle's model. For those, open the Gradle project Kargo generates in `target/android` (see [Android](../android/)) in a second window.

### Limitations of the plugin

- *Rerun Failed Tests* is not available; rerun a single test from its gutter icon.
- The IDE analyzes each module for one platform. In a package with several targets, common code (`src/`) is analyzed like the first target, so completion there also offers that platform's APIs. *Tools → Kargo → Check* shows what fails on the other targets.
- The npm packages of a [path dependency](../workspaces/#path-dependencies) are not described to the IDE: the import doesn't build a path dependency to read the npm packages in its klibs.
- The [Kotlin declarations](../wasm/#kotlin-declarations-for-npm-packages) generated for `npm-dependencies` and for your own [TypeScript modules](../wasm/#your-own-javascript-and-typescript) are not yet a source root of the Wasm and JS modules, so calls into them show as unresolved in the IDE; they compile and run. `kargo lsp` with Kargo's own Kotlin server, in other editors, reads them.
- Compiler plugins from [`[frameworks]`](../frameworks/), such as serialization, are handed to the IDE but it may not apply them, so it can report errors that `kargo build` doesn't. *Tools → Kargo → Check* shows the compiler's own.
- The IDE reads libraries compiled by its own Kotlin version or one newer at most, see the note under [Project import](#project-import).


## Visual Studio Code

The Kargo extension makes [Visual Studio Code](https://code.visualstudio.com) understand Kargo projects. It runs [`kargo lsp`](#language-server) for Kotlin files and `kargo.toml`, runs the commands of Kargo as tasks whose compiler errors appear in the Problems view, shows the tests in a test explorer, puts Run and Debug buttons above `fun main` and the tests, and debugs JVM programs and tests through [`kargo dap`](#debug-adapter).

Open a folder that contains a `kargo.toml` at its root and the extension takes over. You don't need a `build.gradle.kts` or a `pom.xml`.

### Install the extension

The extension needs:

- Kargo itself. See [Installation](../../getting-started/installation/).
- VS Code 1.100 or newer.
The extension highlights Kotlin itself. Another Kotlin extension that runs its own Kotlin language server competes with `kargo lsp`, so turn that server off for Kargo projects.

The extension isn't on the Marketplace yet. Build it from the repository, which needs Node.js 22 or newer:

```sh
cd editors/vscode
npm ci
npm run package
```

`npm run package` writes `kargo-vscode.vsix`. Install it with *Extensions → ⋯ → Install from VSIX…*. Every build on `master` also attaches the package as an artifact of the *VS Code extension* workflow on GitHub Actions.

The extension looks for `kargo` in the `kargo.path` setting, then on `PATH`, then in `~/.kargo/bin` (`$KARGO_HOME/bin`, where the [install script](../../getting-started/installation/) puts it), then in `~/.cargo/bin`. If Kargo is somewhere else, set its path:

```json title="settings.json"
{
  "kargo.path": "/home/me/.cargo/bin/kargo"
}
```

If it can't find Kargo, it says so and links to the installation guide.

### Code intelligence in VS Code

For each folder with a `kargo.toml` at its root, the extension starts `kargo lsp`. It serves the Kotlin and `kargo.toml` files of that folder, so completion, navigation and errors use the project's dependencies, with the features listed in [Language server](#language-server):

- Completion, hover, go to definition, find references, rename, [code actions](../kotlin-server/#code-actions) (the light bulb, and *Organize Imports*), [refactorings](../kotlin-server/#refactorings) (*Refactor...*: extract, inline, change signature, move) and [formatting](../kotlin-server/#formatting) (ktlint, as `kargo fmt`) in Kotlin files, from [Kargo's Kotlin server](../kotlin-server/).
- Completion, documentation, validation and quick fixes in `kargo.toml`, with Maven coordinate and version completion and hints at newer versions. These come from `kargo lsp` itself, so the extension doesn't need a JSON Schema association. A TOML extension that also validates `kargo.toml` adds its own messages.
- [Check on save](#check-on-save): saving a Kotlin or Java file shows the errors of `kargo check` for every target.

| Setting | Default | Meaning |
|---|---|---|
| `kargo.path` | empty | The `kargo` to run. Empty: the one on `PATH`, then `~/.kargo/bin`, then `~/.cargo/bin` |
| `kargo.lsp.server` | `kargo` | The Kotlin server `kargo lsp` runs: `kargo` (Kargo's own), `kotlin-lsp` (JetBrains'), `kls` (fwcd's) or `none`. See [Choosing the Kotlin server](#choosing-the-kotlin-server) |
| `kargo.lsp.checkOnSave` | `true` | Run `kargo check` when a Kotlin or Java file is saved |
| `kargo.lsp.checkTargets` | empty | The targets that check on save compiles. Empty: every enabled target |
| `kargo.lsp.fetchSources` | `true` | Let Kargo's Kotlin server download the sources jars of dependencies |

Changing a setting restarts the language server.

### Tasks and problems

*Tasks: Run Task* lists the `kargo` tasks of the project: `build`, `build --release`, `check --tests`, `test`, `clean`, `fetch --sources` and `update`, then each target's run and test, each [`[[bin]]`](../multiple-programs/), example and task of `kargo.toml`. A task runs in the terminal. Compiler errors and warnings from Kotlin and javac appear in the Problems view under *kargo*, at their line and column.

*Run Build Task* (Ctrl+Shift+B) lists the build tasks. An extension can't choose the default build task, so pick the one you use with *Tasks: Configure Default Build Task*. A task in `tasks.json` with `"type": "kargo"` and an `args` list runs `kargo` with those arguments:

```json title=".vscode/tasks.json"
{
  "version": "2.0.0",
  "tasks": [
    { "label": "Release build", "type": "kargo", "args": ["build", "--release"], "group": "build" }
  ]
}
```

### Run, debug and test

Above each `fun main`, and above each `@Test` function and test class, two buttons run it:

| Button | Runs |
|---|---|
| *Run* | A task in the terminal: `kargo run -t <target>`, `kargo run --bin <name>` for a [`[[bin]]`](../multiple-programs/), or `kargo run --example <name>` in `examples/`. For a test, `kargo test <package.Class.method>`, or the tests of a class |
| *Debug* | The same command under the debugger, through [`kargo dap`](#debug-adapter). Offered for the JVM target |

The target of a file is the one of its directory (`src-wasm/` is Wasm), else the target selected in the [status bar](#status-bar-and-commands).

The test explorer lists the tests of each target, grouped by class. Each test shows its result after a run: passed, failed (with the assertion message, the stack trace and what the test printed) or skipped, and how long it took. Run all the tests, a class, or a selection from the explorer, and *Rerun Failed Tests* runs the failures again. Discovery runs `kargo test --list`, which compiles the tests, when the project opens, when you refresh the explorer, and after you save a test file.

The *Add Configuration…* list of *Run and Debug* offers Kargo's debug configurations: run the program, run the tests, and attach to port 5005. To keep your own, add them to `launch.json`:

```json title=".vscode/launch.json"
{
  "version": "0.2.0",
  "configurations": [
    { "type": "kargo", "request": "launch", "name": "Debug the server", "args": ["run", "--bin", "server"] },
    { "type": "kargo", "request": "launch", "name": "Debug MathTest", "args": ["test", "com.example.app.MathTest"] },
    { "type": "kargo", "request": "attach", "name": "Attach to port 5005", "port": 5005 }
  ]
}
```

`args` are the arguments of `kargo`: Kargo adds `--debug`, and `-t jvm` unless they name a target. The options are listed in [`kargo dap`](../../reference/commands/#kargo-dap), and [Debug adapter](#debug-adapter) describes what Kargo does behind them.

### Status bar and commands

The status bar shows the target and profile that build, run and test use, such as `Kargo: all targets · dev`. Click it, or run *Kargo: Show Menu* from the Command Palette, for Kargo's commands:

| Command | Does |
|---|---|
| Build, Check, Test, Clean | `kargo build`, `kargo check --tests`, `kargo test` and `kargo clean`, in the terminal |
| Run Program… | Runs a program, `[[bin]]` or example of the project, picked from a list |
| Run Task… | Runs a task of `kargo.toml`: `kargo task <name>` |
| Add Dependency… | Searches Maven Central as you type, then runs `kargo add` for the artifact you pick, in `[dependencies]` or `[dev-dependencies]` |
| Remove Dependency… | Runs `kargo remove` for a dependency of `kargo.toml` |
| Fetch Dependencies and Sources | `kargo fetch --sources`, for navigation into library sources |
| Update Dependencies | `kargo update` |
| Select Target… | The target for build, run and test: all enabled targets, or one of `jvm`, `wasm`, `native` and `android`. An Android run asks which device to use in a terminal; set `KARGO_ANDROID_DEVICE` (see [Devices and emulators](../android/#devices-and-emulators)) to choose it without asking |
| Select Profile… | `dev`, or `release` for `--release` |
| Open kargo.toml | The project's manifest |
| Restart the Kargo Language Server | Restarts `kargo lsp` and discovers the tests again |
| Show Kargo Output | The extension's output |

Add Dependency searches the same index as the completion of coordinates in `kargo.toml`. Set `KARGO_MAVEN_SEARCH_URL` to search a mirror of it (see [Environment](../../reference/environment/)).

### Limitations of VS Code

- The extension isn't on the Marketplace yet, so it is installed from a `.vsix` package.
- Kotlin highlighting is syntactic: names are coloured by how they're written (a capitalized name as a type, a name before `(` as a function), not by what they resolve to. With Kargo's own Kotlin server (the default), the editor also gets semantic tokens from the language server, which colour names by what they resolve to.
- The test explorer lists JVM and Native tests. Wasm and Android tests can't be listed yet (see [Test discovery](../../reference/machine-readable-output/#test-discovery)). Debugging works for the JVM target only, and from the Debug button above a test rather than from the explorer.
- Run and Debug buttons are found from the source text, so a `fun main` or a test function the scan doesn't recognize gets no button. Tasks, the test explorer and the commands still work.
- Compiler errors come from the human-readable output of `kargo build`. Android builds run through Gradle, whose errors stay in the terminal.
- An error can appear in the Problems view twice: once from the language server's check on save, under *kargo-lsp*, and once from a build, under *kargo*.
- Debugging uses kotlin-debug-adapter, which doesn't support conditional breakpoints or logpoints, as in [Zed](#debug-in-zed).

## Zed

The Kargo extension makes [Zed](https://zed.dev) understand Kargo projects. It runs [`kargo lsp`](#language-server) for Kotlin files and `kargo.toml`, so completion, navigation and errors use your dependencies, and it debugs programs and tests through [`kargo dap`](#debug-adapter). With the tasks `kargo ide --zed` writes, the run buttons next to `fun main` and your tests run Kargo.

Open a folder that contains a `kargo.toml` and the extension takes over. You don't need a `build.gradle.kts` or a `pom.xml`.

### Install the extension

The extension needs:

- Kargo itself. See [Installation](../../getting-started/installation/).
- Zed's **Kotlin** extension, for syntax highlighting and the run buttons. Install it from *zed: extensions*.
- Rust, installed with [rustup](https://rustup.rs), while the extension isn't in Zed's registry yet: Zed compiles it when you install it.

Install the extension from the repository:

1. Clone Kargo: `git clone https://github.com/enekocm7/kargo`.
2. In Zed, run *zed: install dev extension* from the command palette and pick the `editors/zed` directory.

The extension runs the `kargo` on your `PATH`. If Kargo is somewhere else, set its path in Zed's settings:

```json title="settings.json"
{
  "lsp": {
    "kargo": {
      "binary": { "path": "/home/me/.cargo/bin/kargo" }
    }
  }
}
```

If it can't find Kargo, the Kargo language server fails to start with a message that says so; *zed: open log* shows it.

### Code intelligence in Zed

When you open a Kotlin file or `kargo.toml` in a folder with a `kargo.toml` at its root, Zed starts `kargo lsp` there. You get everything listed in [Editor integration](#language-server):

- Completion, hover, go to definition (into the standard library, the JDK, and library sources, which the server downloads in the background, or stubs generated from the compiled library until then and for libraries without sources), find references, rename, [code actions](../kotlin-server/#code-actions), [refactorings](../kotlin-server/#refactorings) and [formatting](../kotlin-server/#formatting) in Kotlin files, from [Kargo's Kotlin server](../kotlin-server/). It reads your project the way `kargo build` compiles it, and again when `kargo.toml` or `kargo.lock` change.
- Completion, documentation, validation and quick fixes in `kargo.toml`, with Maven coordinate and version completion and hints at newer versions. The quick fixes add dependencies, frameworks and targets the way `kargo add` and `kargo target add` do.
- [Check on save](#check-on-save): when you save a Kotlin file, the errors of `kargo check` for every target appear in the editor and in *diagnostics: deploy*. The extension turns it on.

The first start downloads the standard library's sources, which takes a moment. To use JetBrains' Kotlin LSP instead, add `"--server", "kotlin-lsp"` to `binary.arguments` (see [Zed settings](#zed-settings)); its first start downloads the server and resolves your dependencies, so it takes a while.

Zed's Kotlin extension brings its own Kotlin language servers, which don't know your dependencies. Use Kargo's instead:

```json title="settings.json"
{
  "languages": {
    "Kotlin": {
      "language_servers": ["kargo", "!kotlin-lsp", "!kotlin-language-server", "..."]
    }
  }
}
```

Put this in the project's `.zed/settings.json` to change only Kargo projects.

#### Zed settings

`kargo lsp` reads its options from `lsp.kargo`:

```json title="settings.json"
{
  "lsp": {
    "kargo": {
      "binary": { "arguments": ["--server", "kotlin-lsp"] },
      "initialization_options": { "checkOnSave": true, "checkTargets": ["wasm", "native"] }
    }
  }
}
```

| Setting | Default | Meaning |
|---|---|---|
| `binary.path` | the `kargo` on `PATH` | The `kargo` to run |
| `binary.arguments` | none | Added after `kargo lsp`: [`--server kotlin-lsp`, `--server kls`, `--server none`, `--klib-targets`](#choosing-the-kotlin-server) |
| `binary.env` | none | Environment variables for `kargo lsp` |
| `initialization_options.checkOnSave` | `true` | Run `kargo check` when a Kotlin or Java file is saved |
| `initialization_options.checkTargets` | every enabled target | The targets check on save compiles |
| `initialization_options.fetchSources` | `true` | Let [Kargo's Kotlin server](../kotlin-server/#libraries) download the sources jars of dependencies |

### Run and test in Zed

Zed's Kotlin extension shows a run button next to `fun main`, each `@Test` function and each test class. What the button runs comes from your tasks. Write Kargo tasks for the project once:

```sh
kargo ide --zed
```

This writes `.zed/tasks.json`. Then the buttons run Kargo:

| Next to | Runs |
|---|---|
| A `fun main` | A program of the package: `kargo run -t <target>`, `kargo run --bin <name>` for a [`[[bin]]`](../multiple-programs/), `kargo run --example <name>`; Zed lets you pick when there are several |
| A test function | That test: `kargo test <package.Class.method>` |
| A test class | Its tests: `kargo test <package.Class>` |

Run tests from the directory of the file, so in a [workspace](../workspaces/) they run in the member that contains it.

*task: spawn* lists the other tasks: each target's program and tests (`kargo run -t wasm`, `kargo test -t native`), each [task](../tasks-and-plugins/) in `kargo.toml` (`kargo task <name>`), and `kargo build`, `build --release`, `check --tests`, `run --hot-reload` (run with [hot reload](../watch/#hot-reload-a-jvm-program), from the package of the open file), `test`, `clean`, `fetch --sources`, `update` and `outdated`. Select `group:artifact` coordinates in an editor and run `kargo add $ZED_SELECTED_TEXT` to add them as a dependency, or `kargo remove $ZED_SELECTED_TEXT` to remove one. Bind any task to a key with the `task::Spawn` action and its `task_name`.

Kargo keeps the file current: after you change `kargo.toml`, `kargo lsp` and commands such as `kargo build` and `kargo add` write it again, so a new `[[bin]]`, target or task gets its task. Kargo never replaces a `.zed/tasks.json` it didn't write, so to keep your own tasks next to Kargo's, put yours in Zed's global tasks (*zed: open tasks*). If you edit the generated file, Kargo stops updating it; delete it and run `kargo ide --zed` to get it back.

Compiler errors in a task's output name the file, line and column; Cmd-click (Ctrl-click on Linux and Windows) one in the terminal to open it.

### Debug in Zed

The extension debugs JVM programs and tests:

- **From a run button**: its menu offers *Debug* for each `kargo run` and `kargo test` task. The program starts under `kargo dap` and stops at your breakpoints.
- **From a task**: *debugger: start* lists the same tasks, converted by the extension's `kargo` locator.
- **From `.zed/debug.json`**, for configurations you keep:

  ```json title=".zed/debug.json"
  [
    { "label": "Debug the server", "adapter": "Kargo", "request": "launch", "args": ["run", "--bin", "server"] },
    { "label": "Debug MathTest", "adapter": "Kargo", "request": "launch", "args": ["test", "com.example.app.MathTest"] },
    { "label": "Attach to port 5005", "adapter": "Kargo", "request": "attach", "port": 5005 }
  ]
  ```

`args` are the arguments of `kargo`; Kargo adds `--debug`, and `-t jvm` unless they name a target. Breakpoints, stepping, the call stack and variables work; the debug console shows what the program prints. The options are listed in [`kargo dap`](../../reference/commands/#kargo-dap), and what Kargo does behind them in [Debug adapter](#debug-adapter).

`kargo dap` downloads kotlin-debug-adapter on first use and runs it on the project's JDK.

### Assistant context

Type `/kargo-metadata` in a text thread of the assistant panel to insert the project as Kargo sees it: each package's targets with their source and test directories, its programs, examples and tasks, and its `kargo.toml` with the dependencies and frameworks. The assistant then knows where code goes and which libraries it can use.

### `kargo.toml` in Zed

`kargo lsp` validates `kargo.toml` against Kargo's [JSON Schema](#completion-and-validation-in-kargotoml) and Kargo's own rules, so you don't need anything else. If you also use Zed's TOML language server (Taplo), add `#:schema https://enekocm7.github.io/kargo/schemas/kargo.json` as the first line of `kargo.toml` for Taplo's completion too, or limit `kargo.toml` to Kargo's server.

### Kargo scripts in Zed

The extension starts `kargo lsp` for a folder with a `kargo.toml` at its root. A folder with only [Kotlin scripts](../scripts/) has no `kargo.toml`, so the server stays off there unless you turn it on for that folder:

```json title="Zed settings.json"
{
  "lsp": {
    "kargo": {
      "settings": { "scripts": true }
    }
  }
}
```

With that setting, Zed runs `kargo lsp` in the folder, and it completes, documents and checks the `@file:` annotations of the Kargo scripts in it. It stays off by default, because in a folder that another tool manages the server would take over the Kotlin files as well.

### Limitations of Zed

- Zed has no test explorer: run tests from their buttons or tasks, and read the results in the terminal. A failed test is re-run from its button.
- The extension starts `kargo lsp` only when `kargo.toml` is at the root of the folder you open. Open the project root, not a directory above it.
- Code in `src-wasm/`, `src-native/` and `android/` gets no Kotlin code intelligence from the Kotlin LSP yet. Check on save still reports its errors.
- Debugging works for the JVM target only. kotlin-debug-adapter doesn't support conditional breakpoints or logpoints, and evaluates variable names only.
- Slash commands work in text threads; the agent panel's threads don't run them.


## Neovim

Kargo's Neovim plugin, in the `editors/neovim` directory of the repository, runs `kargo lsp` for Kotlin and `kargo.toml`, runs the Kargo commands with their errors in the quickfix list, lists the runnables of a package, shows the tests in [neotest](https://github.com/nvim-neotest/neotest), and debugs programs and tests with [nvim-dap](https://github.com/mfussenegger/nvim-dap). It needs Neovim 0.11 or newer and `kargo` on your `PATH`, see [Installation](../../getting-started/installation/).

### Install the Neovim plugin

With [lazy.nvim](https://github.com/folke/lazy.nvim), point the plugin at its subdirectory of the repository:

```lua title="init.lua"
{
  'enekocm7/kargo',
  subdir = 'editors/neovim',
  opts = {},
}
```

Then enable the language server, which the plugin ships as `lsp/kargo.lua`:

```lua title="init.lua"
vim.lsp.enable('kargo')
```

The server starts for Kotlin and TOML files in a directory with a `kargo.lock` or `kargo.toml`, and imports the project as [Kargo's Kotlin server](../kotlin-server/) does. Saving `kargo.toml` re-imports it. Without the plugin, the server config alone is enough:

```lua title="init.lua"
vim.lsp.config('kargo', {
  cmd = { 'kargo', 'lsp' },
  filetypes = { 'kotlin', 'toml' },
  root_markers = { 'kargo.lock', 'kargo.toml' },
})
vim.lsp.enable('kargo')
```

`setup()` takes two options, both optional:

| Option | Default | Meaning |
|---|---|---|
| `cmd` | `'kargo'` | The `kargo` executable, for the language server, the commands, tests and `kargo dap` |
| `height` | `12` | The rows of the terminal split that commands run in |

```lua title="init.lua"
require('kargo').setup({ cmd = vim.fn.expand('~/.cargo/bin/kargo') })
```

### Commands

`:Kargo` runs a Kargo command in the project of the current buffer, the nearest directory with a `kargo.toml`:

| Command | Does |
|---|---|
| `:Kargo build [args]` | Runs `kargo build --message-format json`. Compiler errors and warnings go to the quickfix list, titled `kargo build`, and are shown as diagnostics in their files |
| `:Kargo check [args]` | The same for `kargo check` |
| `:Kargo test [filter]` | Runs `kargo test --message-format json`. Failing tests go to the quickfix list, and a notification gives the counts |
| `:Kargo add [coords]` | Adds dependencies through the language server, which edits `kargo.toml` in the buffer. Without coordinates it asks for them. Without a running server it runs `kargo add` in a terminal |
| `:Kargo runnables` | Picks one of the programs, tests and tasks of the buffer's package and runs it in a terminal split. The picker is `vim.ui.select`, so Telescope, fzf-lua or snacks.picker show it when they override it |
| `:Kargo <command> [args]` | Any other command, such as `run`, `clean`, `update`, `remove`, `task` or `tree`, runs in a terminal split. Buffers are reloaded when it exits, since `kargo remove` changes `kargo.toml` |

`build`, `check` and `test` don't open a terminal, so their output is the messages Kargo prints as JSON. The status lines they print go to a notification when they fail without a diagnostic, for example a `kargo.toml` that doesn't parse.

Map the commands you use often:

```lua title="init.lua"
vim.keymap.set('n', '<leader>kb', '<cmd>Kargo build<cr>', { desc = 'Kargo build' })
vim.keymap.set('n', '<leader>kt', '<cmd>Kargo test<cr>', { desc = 'Kargo test' })
vim.keymap.set('n', '<leader>kr', '<cmd>Kargo runnables<cr>', { desc = 'Kargo runnables' })
```

### Tests with neotest

The plugin includes a neotest adapter, `neotest-kargo`. It finds the tests of the Kotlin files under `tests/`, under `tests-<target>/` and in files named `*Test.kt` or `*Tests.kt`: a `fun` below a `@Test` or `@ParameterizedTest` annotation, in the class declared above it. Discovery reads the file and doesn't compile it, so the tests of a file appear as you type, and [`kargo test --list`](../../reference/machine-readable-output/#test-discovery) stays the authority for what runs.

```lua title="init.lua"
require('neotest').setup({
  adapters = { require('neotest-kargo') },
})
```

Each test runs with `kargo test --message-format json` and reports its result on its line. A class runs its tests, and a file runs the tests of its package, since `kargo test` filters by a substring of the qualified name. A failure shows the assertion message at the line in the test file that the stack trace names.

:::note
The adapter finds tests by reading the source, so it doesn't know what the compiler makes of it. A test in a nested class gets its inner class's name without the outer one, which `kargo test` doesn't match, and tests that a build plugin generates don't appear at all. `kargo test --list` lists what Kargo actually runs.
:::

### Debugging with nvim-dap

The plugin registers a `kargo` adapter with nvim-dap, which runs [`kargo dap`](#debug-adapter) in the project of the buffer. Call its setup once after nvim-dap is loaded:

```lua title="init.lua"
require('kargo.dap').setup()
```

It adds three configurations for Kotlin buffers: `kargo run` and `kargo test`, which launch the program or the tests with `--debug`, and `Attach to port 5005`, for a JVM started with `kargo run --debug`. Start one with `:lua require('dap').continue()` and pick it. Debugging works for JVM programs and tests, as described in [Debugging](#debugging) and [Debug adapter](#debug-adapter) below.

### Limitations of Neovim

- There are no Run and Debug code lenses above `main` and the tests. Use `:Kargo runnables`, the neotest adapter, or `require('dap').continue()` instead.
- There is no command to switch the active target. Pass `-t <target>` to a command, such as `:Kargo build -t wasm`.
- The plugin isn't in the Mason registry yet. Install `kargo` yourself, as described in [Installation](../../getting-started/installation/).

## Emacs

Kargo's Emacs package, in the `editors/emacs` directory of the repository, runs [`kargo lsp`](#language-server) from Eglot or lsp-mode for Kotlin and `kargo.toml`, and runs the Kargo commands from Emacs through `compile`, so `next-error` jumps to compiler errors and to the line of a failed test. It lists the result of each test, shows build errors in Flymake, debugs programs and tests with [Dape](https://github.com/svaante/dape) through [`kargo dap`](#debug-adapter), and has a Transient menu of all commands. It needs Emacs 29.1 or newer and `kargo` on your `exec-path`, see [Installation](../../getting-started/installation/).

### Install the Emacs package

The package isn't on MELPA yet. Install it from the repository; Emacs 29 and newer can do that with `package-vc`:

```elisp title="init.el"
(package-vc-install '(kargo :url "https://github.com/enekocm7/kargo" :lisp-dir "editors/emacs"))
```

Or clone Kargo and add the directory to your `load-path`:

```elisp title="init.el"
(add-to-list 'load-path "~/src/kargo/editors/emacs")
(require 'kargo)
```

Turn on `kargo-mode`, which binds the commands under `C-c k` in Kotlin and `kargo.toml` buffers of a Kargo project:

```elisp title="init.el"
(global-kargo-mode 1)
;; or, for the Kotlin modes only:
(add-hook 'kotlin-mode-hook #'kargo-mode)
(add-hook 'kotlin-ts-mode-hook #'kargo-mode)
```

The package works with `kotlin-mode` (MELPA) and `kotlin-ts-mode`, and with `conf-toml-mode`, `toml-mode` and `toml-ts-mode` for `kargo.toml`. If Emacs is started from the Dock or a launcher and can't find `kargo` (it isn't in `exec-path`), give its path in `kargo-executable`, or set up `exec-path` from your shell with [exec-path-from-shell](https://github.com/purcell/exec-path-from-shell):

```elisp title="init.el"
(setq kargo-executable (expand-file-name "~/.cargo/bin/kargo"))
```

A command that can't find the executable stops with a message that says so and names `kargo-executable`.

### Language server in Emacs

With **Eglot**, which ships with Emacs, register `kargo lsp` once:

```elisp title="init.el"
(setq kargo-eglot-auto-start t)   ; optional: start Eglot in Kotlin files and kargo.toml of a Kargo project
(with-eval-after-load 'eglot
  (require 'kargo-eglot)
  (kargo-eglot-setup))
```

`kargo-eglot-setup` adds an `eglot-server-programs` entry for `kotlin-mode`, `kotlin-ts-mode` and the TOML modes that runs `kargo lsp`, and adds `kargo.toml` to `project-vc-extra-root-markers`, so `project.el` (and so Eglot) takes the directory of a `kargo.toml` as the project root, also in a workspace member and without version control. Without `kargo-eglot-auto-start`, run `M-x eglot` in a buffer. `kargo lsp` gives features for `kargo.toml` alone, so other TOML files are unaffected. To leave TOML to another server, such as Taplo, drop the TOML modes before calling `kargo-eglot-setup`:

```elisp title="init.el"
(setq kargo-eglot-modes '(kotlin-mode kotlin-ts-mode))
```

With **lsp-mode**, load the client and start lsp-mode in the buffers you want:

```elisp title="init.el"
(with-eval-after-load 'lsp-mode
  (require 'lsp-kargo))
(add-hook 'kotlin-mode-hook #'lsp-deferred)
(add-hook 'kotlin-ts-mode-hook #'lsp-deferred)
(add-hook 'conf-toml-mode-hook #'lsp-deferred)
```

`lsp-kargo` registers the server `kargo`. It is active only where a `kargo.toml` is above the file, and then it outranks other Kotlin clients; it serves TOML files named `kargo.toml` only, so lsp-mode's TOML client keeps the others.

Both clients start `kargo lsp` with these options:

| Variable | Default | Meaning |
|---|---|---|
| `kargo-executable` | `"kargo"` | The `kargo` executable, for the language server and for every command |
| `kargo-lsp-server` | `nil` | The Kotlin server behind `kargo lsp`: `nil` for [Kargo's own](../kotlin-server/), `"kotlin-lsp"` for JetBrains' Kotlin LSP (`--server kotlin-lsp`), `"kls"`, or `"none"`. See [Choosing the Kotlin server](#choosing-the-kotlin-server) |
| `kargo-lsp-server-path` | `nil` | A server executable to run instead of the one Kargo downloads (`--server-path`) |
| `kargo-lsp-klib-targets` | `nil` | Also describe Wasm and Native code to the Kotlin LSP (`--klib-targets`) |
| `kargo-lsp-check-on-save` | `default` | `t` or `nil` turn [check on save](#check-on-save) on or off; `default` leaves it to the server (on with Kargo's own Kotlin server) |
| `kargo-lsp-check-targets` | `nil` | The targets check on save compiles, such as `("wasm" "native")`; `nil` is every enabled target |

```elisp title="init.el"
(setq kargo-lsp-server "kotlin-lsp")
```

A change applies when the server starts, so restart it with `M-x eglot-reconnect` or `M-x lsp-workspace-restart`.

What the server gives Kotlin and `kargo.toml` is described in [Language server](#language-server); in Emacs it is the usual commands of the client:

| You want | Eglot | lsp-mode |
|---|---|---|
| Completion, with imports added | `completion-at-point` | `completion-at-point` |
| Go to definition, also into library sources, and find usages | `xref-find-definitions`, `xref-find-references` (`M-.`, `M-?`) | the same |
| Hover documentation | Eldoc | Eldoc or `lsp-ui-doc` |
| Rename | `eglot-rename` | `lsp-rename` |
| Quick fixes and import actions | `eglot-code-actions` | `lsp-execute-code-action` |
| Errors and warnings | Flymake | Flycheck or Flymake |

Errors and warnings from [check on save](#check-on-save) appear in the same list as the server's own, with the ranges the compiler reported. Saving `kargo.toml` or `kargo.lock` imports the project again. Run `M-x kargo-fetch` with a prefix argument (`C-u C-c k g`) once to download the sources of your dependencies, as described in [Library sources](#library-sources). Kargo's own Kotlin server has no formatting and no signature help yet ([its limits](../kotlin-server/#limits)): format with `M-x kargo-command RET fmt`.

### Commands

Every command runs in a `compilation-mode` buffer named `*kargo <command>: <project>*`, in the directory of the nearest `kargo.toml` above the current buffer. `g` runs the command again there. A prefix argument asks for extra arguments, so `C-u M-x kargo-run` can pass `--bin server`.

| Command | Key | Does |
|---|---|---|
| `kargo-build` | `C-c k b` | `kargo build` |
| `kargo-check` | `C-c k c` | `kargo check`, with `--tests` while `kargo-check-tests` is `t` (the default) |
| `kargo-run` | `C-c k r` | `kargo run`. Its output is shown in the buffer; the program can't read from the keyboard |
| `kargo-clean` | `C-c k C` | `kargo clean` |
| `kargo-add` | `C-c k a` | `kargo add` for the coordinates you type (`group:artifact[:version]`, several separated by spaces); with a prefix argument it adds to `[dev-dependencies]` |
| `kargo-remove` | `C-c k d` | `kargo remove`, completing the dependencies of `kargo.toml` |
| `kargo-update`, `kargo-fetch`, `kargo-tree` | `C-c k u`, `g`, `e` | `kargo update`, `fetch` (with a prefix argument `--sources`) and `tree` |
| `kargo-task` | `C-c k j` | Runs a task of `kargo.toml`, completing their names |
| `kargo-runnables` | `C-c k l` | Picks one of the programs, tests and tasks of the package and runs it. It asks the language server for [`kargo/runnables`](#runnables) when it is running, else reads `kargo metadata` |
| `kargo-set-target` | `C-c k T` | Chooses the target (`jvm`, `wasm`, `native`, `android`) that `build`, `check`, `run` and `test` pass as `-t`, or `all`. The mode line shows it |
| `kargo-toggle-release` | `C-c k R` | Switches between the dev and the release profile |
| `kargo-target-add` | | `kargo target add` |
| `kargo-command` | `C-c k x` | Any `kargo` command, such as `fmt --check` or `lint` |
| `kargo-menu` | `C-c k m` | A [Transient](https://github.com/magit/transient) menu of all of them |

After `add`, `remove`, `update`, `fetch` or `target add`, Emacs reloads the `kargo.toml` and `kargo.lock` buffers that have no unsaved changes, and the language server imports the project again.

`C-c k` is the default prefix. To change it, set `kargo-keymap-prefix` before `kargo` loads, for example `(setq kargo-keymap-prefix (kbd "C-c C-k"))`.

The package also makes Kargo known to `project.el` and Projectile: `(kargo-setup-project)` makes `project.el` use the directory of a `kargo.toml` as a project root (`kargo-eglot-setup` does it), and Projectile gets a `kargo` project type, with `kargo build`, `kargo test` and `kargo run` as its compile, test and run commands, when it loads.

### Compiler errors in Emacs

`kargo-build`, `check`, `run` and `test` ask Kargo for [JSON messages](../../reference/machine-readable-output/#compiler-diagnostics) and show each compiler error as `file:line:column: error: message` with the absolute path, followed by the source line and a caret. `next-error` and `previous-error` (`M-g n`, `M-g p`) and a click or `RET` on a line open the file at the line and column the compiler gave. Warnings are warnings and notes are notes, so `next-error` skips the notes by default (`compilation-skip-threshold`).

To see the errors of the last command in the buffers of their files, turn on Flymake with the Kargo backend:

```elisp title="init.el"
(add-hook 'kotlin-mode-hook #'kargo-flymake-setup)
(add-hook 'kotlin-ts-mode-hook #'kargo-flymake-setup)
```

When `kargo-build`, `check`, `run` or `test` ends, Flymake shows its errors in each file, `M-x flymake-show-project-diagnostics` lists them, and the next command replaces them. With `(setq kargo-flymake-check-on-save t)`, saving a file also runs `kargo check` in the background. The language server does that already with Kargo's own Kotlin server, so use either the backend's check on save or the server's, for example with `(setq kargo-lsp-check-on-save nil)`.

### Tests in Emacs

`kargo-test` runs `kargo test`; the others run one test, a class or a file by its qualified name, which is how `kargo test` filters:

| Command | Key | Runs |
|---|---|---|
| `kargo-test` | `C-c k t` | Every test of the package |
| `kargo-test-at-point` | `C-c k .` | The test function at or above point |
| `kargo-test-class` | `C-c k k` | The tests of the class at or above point |
| `kargo-test-file` | `C-c k f` | The tests of every class in the file |
| `kargo-test-pick` | `C-c k p` | A test picked from [`kargo test --list`](#test-explorer), which compiles the tests, so it takes a moment |
| `kargo-test-rerun-failed` | `C-c k F` | The tests that failed in the last run, each in its own `kargo test` |
| `kargo-test-results` | `C-c k v` | Shows the results of the last run again |

The first three find the test by reading the Kotlin source: a `fun` below a `@Test`, `@ParameterizedTest` or `@RepeatedTest` annotation, in the class declared above it. It doesn't compile the file, so a test in a nested class gets its inner class's name without the outer one, which `kargo test` doesn't match, and tests that a build plugin generates aren't found. `kargo-test-pick` shows what Kargo actually runs.

When the tests end, a `*kargo tests: <project>*` buffer lists every test with `pass`, `FAIL` or `skip` and its time. In it, `RET` opens the test's source, `d` shows the failure message and stack trace, `r` runs that test again and `R` runs all failed tests again. In the compilation buffer, the frames of a failure's stack trace that are in your sources are links, and `next-error` goes to the first one, usually the failed assertion. `kargo-test-display-results` says when the results buffer opens: always (the default), only when a test failed (`failures`) or never (`nil`).

### Debugging with Dape

[Dape](https://github.com/svaante/dape) is the debug adapter client that works with `kargo dap`, which builds and starts the program with `--debug`, shows its output in the debug console and attaches once the JVM is ready, so a breakpoint on the first line of `main` is hit. Register the configurations once Dape has loaded:

```elisp title="init.el"
(with-eval-after-load 'dape
  (require 'kargo-dape)
  (kargo-dape-setup))
```

That adds `kargo` (the program), `kargo-test` (the tests) and `kargo-attach` (a JVM started with `kargo run --debug`, on `kargo-dape-attach-port`, 5005 by default) to `dape-configs`, so `M-x dape` offers them. These commands start them directly:

| Command | Debugs |
|---|---|
| `kargo-dape-run` | The program. A prefix argument asks for arguments, such as `--bin server` |
| `kargo-dape-test-at-point` | The test at point, or every test when point is not in one |
| `kargo-dape-attach` | A program you started with `kargo run --debug` |

Breakpoints, stepping and variables work as described in [Debugging](#debugging) and [Debug adapter](#debug-adapter): for JVM programs and tests, with the project of the buffer as the working directory. dap-mode can't launch `kargo dap`: it connects to debug adapters over a TCP port, and `kargo dap` speaks over stdin and stdout.

### `kargo.toml` in Emacs

`kargo lsp` checks `kargo.toml` against the [JSON Schema](#completion-and-validation-in-kargotoml) as you type, completes keys and values, shows documentation on hover, completes Maven coordinates and versions and hints at newer versions; see [Language server](#language-server). When Taplo is your TOML server instead (leave the TOML modes out of `kargo-eglot-modes`, see above), put the schema comment on the first line of `kargo.toml` so that Taplo validates against it:

```toml title="kargo.toml"
#:schema https://enekocm7.github.io/kargo/schemas/kargo.json
```

and register Taplo for your TOML mode:

```elisp title="init.el"
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs '(toml-ts-mode . ("taplo" "lsp" "stdio"))))
```

### Limitations of Emacs

- The package isn't on MELPA yet, so install it from the repository as described above.
- There are no gutter icons next to tests. Use `kargo-test-at-point`, or the results buffer.
- Debugging needs Dape; dap-mode can't launch `kargo dap`. A JVM that waits for a debugger (`kargo run --debug`) can still be attached with any JDWP client.
- `kargo-run` shows the program's output in a read-only buffer, so a program that reads from the keyboard has no input. Run it in a terminal (`M-x term` or `M-x shell`) when it needs input.
- Android sources are built through Gradle, whose errors are not converted into links.

## Other editors

Any editor with an LSP client can run `kargo lsp`. These are the setups we document, next to [Neovim](#neovim) and [Emacs](#emacs) above:

**Helix**:

```toml title="~/.config/helix/languages.toml"
[language-server.kargo]
command = "kargo"
args = ["lsp"]

[[language]]
name = "kotlin"
language-servers = ["kargo"]

[[language]]
name = "toml"
language-servers = ["kargo", "taplo"]
```

Any other editor with a generic LSP client works the same way: run `kargo lsp` for Kotlin and TOML files. Kargo answers for every TOML file it is sent but only gives `kargo.toml` features, so other TOML files are unaffected.

## Language server

`kargo lsp` is one language server for a whole Kargo project. Point your editor at it for Kotlin files and `kargo.toml`:

```sh
kargo lsp
```

It speaks LSP over stdin and stdout and gives you:

- **Kotlin code intelligence** (completion, hover, signature help, go to definition and implementation, references, call and type hierarchies, rename, symbols, semantic highlighting, folding, selection ranges, [formatting](../kotlin-server/#formatting) with ktlint, and [code actions](../kotlin-server/#code-actions) that import names, organize imports, add a missing dependency, implement members, add the missing branches of a `when` and add `actual` declarations, and [refactorings](../kotlin-server/#refactorings): extract a variable or function, inline a variable or function, move or remove a parameter, move a declaration to another package or file) from [Kargo's Kotlin server](../kotlin-server/), built into `kargo`. Nothing is downloaded but the standard library's sources, and it needs no JDK. With [`--server kotlin-lsp`](#choosing-the-kotlin-server), JetBrains' Kotlin LSP gives it instead, with the compiler's own analysis: Kargo downloads that server on first use, describes your project to it, and passes Kotlin traffic through unchanged, with one exception: an editor that doesn't show a completion item's `labelDetails` gets the signature as the item's `detail`, so each overload of `print` shows its own parameters instead of a bare `print` repeated.
- **`kargo.toml` support**: completion of keys, tables and values, documentation on hover, and errors as you type. Errors come from the [JSON Schema](#completion-and-validation-in-kargotoml) first (unknown keys with a "did you mean", wrong types, invalid values), then from Kargo's own validation and [warnings](../../reference/warnings/), the same messages `kargo build` prints.
- **Dependencies in `kargo.toml`**: completion of `group:artifact` coordinates from Maven Central and of versions from your repositories, and a hint on every dependency with a newer version. See [Dependency completion and update hints](#dependency-completion-and-update-hints).
- **Code actions in `kargo.toml`**: quick fixes for its errors, and the edits of `kargo add`, `kargo add --framework`, `kargo target add` and `kargo upgrade` from the editor. See [Code actions in `kargo.toml`](#code-actions-in-kargotoml).
- **Re-import** when `kargo.toml` or `kargo.lock` change: on save, or when the editor reports a changed file (Kargo asks to be told about every `kargo.toml` and `kargo.lock`). If the project's [fingerprint](#importing-a-project) changed, Kargo resolves the dependencies again in the background, reports it with `$/progress`, and the Kotlin server reloads. Add a dependency with `kargo add` and its classes complete a moment later.
- **Runnables**: the [`kargo/runnables`](#runnables) request lists programs, tests and tasks with the command lines that run them, for Run and Debug buttons.
- **Test tree**: the [`kargo/testTree`](#test-tree) request lists the test classes and functions of the project, each with the command line that runs it, for editors without a test plugin.
- **Typing aids**: on-type formatting (indentation after Enter, ` * ` in comments, closing `"""`, `*/` and brackets the editor didn't add), linked editing of a type parameter and its uses in one signature, links for URLs and for strings that name existing files, inline values while debugging, renamed and deleted files leaving the index at once, and a *Run `kargo fmt` on file* code action. See [Typing aids](../kotlin-server/#typing-aids).
- **Status and logs**: `kargo/status` notifications while the project is indexed, resolved or checked, and `--log-file` with the time of every request. See [Status and logs](#status-and-logs).
- **Check on save**: when you save a Kotlin file, `kargo check` runs in the background and its errors for every target (Wasm and Native included) appear in the editor. It is on with Kargo's own server, which reports only syntax problems, unused code and deprecated usages of its own, and opt-in with the others. See [Check on save](#check-on-save).

With `--server kotlin-lsp`, the first start downloads the Kotlin LSP (about 360 MB) and resolves the project's dependencies, so it takes a while. Later starts reuse both.


:::note
Open the project root or any directory in it. Kargo finds the project from the folder the editor opens and points the Kotlin server at the project root, so opening a workspace member works too.
:::

### Choosing the Kotlin server

| Command | Kotlin server |
|---|---|
| `kargo lsp` | [Kargo's Kotlin server](../kotlin-server/) (the default), built in |
| `kargo lsp --server kargo` | The same, spelled out |
| `kargo lsp --server kotlin-lsp` | JetBrains' [Kotlin LSP](https://github.com/Kotlin/kotlin-lsp): the Kotlin compiler's analysis, quick fixes and formatting, for JVM code |
| `kargo lsp --server kls` | fwcd's [kotlin-language-server](https://github.com/fwcd/kotlin-language-server), for setups that still use it. It runs on the project's JDK, or on a Temurin 21 Kargo installs when that is newer than 21 |
| `kargo lsp --server none` | No Kotlin server: only `kargo.toml` support and runnables |
| `kargo lsp --server-path <path>` | The server executable at `<path>` instead of the one Kargo downloads: a Kotlin LSP, or a kotlin-language-server with `--server kls` |
| `kargo lsp --klib-targets` | Also describe Wasm and Native code to the Kotlin LSP. See [Multiplatform projects](#multiplatform-projects) |

Kargo's own server knows your sources and the libraries' sources but is not the Kotlin compiler; see [its limits](../kotlin-server/#limits). The Kotlin LSP analyzes like the compiler, but only JVM code, and it is a large download that runs a JVM. kotlin-language-server is no longer maintained and its last release bundles Kotlin 2.1, so it understands code and libraries built with newer Kotlin versions only partly; prefer the Kotlin LSP over it.

Kargo pins the version of the Kotlin LSP and kotlin-language-server and installs them like any other [toolchain](../toolchains/#kotlin-language-servers). `kargo toolchain install kotlin-lsp` downloads it ahead of time; `kargo --offline lsp` uses what is installed and the dependencies already in the cache.

### Files it writes

Kargo's own server reads the project itself and writes nothing. The Kotlin LSP and kotlin-language-server don't read `kargo.toml`, so for them Kargo writes the project description each one understands at the project root, from [`kargo metadata`](#importing-a-project):

| Server | File | Contents |
|---|---|---|
| Kotlin LSP | `workspace.json` | A module per package for main code and one for tests (a module per source set for a [multiplatform](#multiplatform-projects) package), with their source and resource roots, the JVM target's classpaths as libraries (with sources jars once downloaded with `kargo fetch --sources`), path dependencies as modules, the JDK, `jvm-target`, compiler arguments and compiler plugins |
| kotlin-language-server | `kls-classpath` (`kls-classpath.bat` on Windows) | A script printing the test classpath, which Kargo keeps in `target/lsp/classpath.txt` |

[`kargo ide`](#without-kargo-lsp) writes the same files without running a server. They hold paths on your machine, so keep them out of version control; `kargo new` adds them to `.gitignore`. Kargo only replaces a file while it still holds what Kargo last wrote: a `workspace.json` you wrote yourself, or one another tool generated, is left alone, and `kargo lsp` tells you so. Delete it to let Kargo manage it. Kargo remembers the last complete import in `target/lsp`, so a restart with an unchanged `kargo.toml` and `kargo.lock` starts the Kotlin server right away.

When a folder has both, the Kotlin LSP imports an IDE's `.idea/` (IntelliJ) instead of `workspace.json`, and `.idea/` describes none of your libraries. `kargo lsp` therefore tells the server to use `workspace.json` for the project (the `buildTools` initialization option, unless your editor sets its own), so you can keep IntelliJ's `.idea/` next to Zed or another editor. This does not apply to a server you start yourself; see [Without `kargo lsp`](#without-kargo-lsp).

If the dependencies can't be resolved (offline, a broken `kargo.toml`), Kargo describes the project without them and says why. It imports again once `kargo.toml` or `kargo.lock` change.

### Multiplatform projects

Kargo's own server reads every target's sources and links `expect` and `actual` declarations, so this section is about the Kotlin LSP; see [How it reads your project](../kotlin-server/#how-it-reads-your-project) for Kargo's.

In a package with several [targets](../multiplatform/), `workspace.json` has a module per source set, the way Gradle describes a Kotlin Multiplatform project to IntelliJ:

| Module | Sources | Refines |
|---|---|---|
| `<name>.common.main` | `src/` | |
| `<name>.jvm.main` | `src-jvm/`, `resources/` | `<name>.common.main` |
| `<name>.common.test` | `tests/` | |
| `<name>.jvm.test` | `tests-jvm/`, `test-resources/` | `<name>.common.test` |

Because `src-jvm/` refines `src/`, an `expect` declaration and its `actual` find each other: go to implementation on an `expect` lists its `actual` declarations, and neither side reports a missing or duplicate declaration. A package with one target keeps one module for its main code and one for its tests.

The Kotlin LSP analyzes JVM code only. Kargo therefore analyzes `src/` as JVM code with the JVM classpath, so common code resolves the standard library and your dependencies, but completion in `src/` also offers JVM-only APIs. `kargo check` still compiles `src/` for every target and reports what doesn't exist on one of them; turn on [check on save](#check-on-save) to see those errors in the editor.

`kargo lsp --klib-targets` also writes `<name>.wasm.main`, `<name>.native.main` and their test modules, with their klibs and, for Native, the Kotlin/Native distribution's standard and platform libraries. The Kotlin LSP Kargo pins doesn't support them yet: it can't read klibs, so library calls in `src-wasm/` show as unresolved, and it rejects Native modules. Use the option with a newer server (`--server-path`) that supports them. Without a JVM target, `src/` is then analyzed for all of the package's platforms, with no libraries.

:::caution
Code that only exists for Wasm, Native or Android (in `src-wasm/`, `src-native/` or `android/`) gets no Kotlin code intelligence from the Kotlin LSP yet; Kargo's own server, the default, reads it. Use Android Studio for Android code. `kargo.toml` support, runnables and check on save work for every target.
:::

### Without `kargo lsp`

If your editor already runs JetBrains' Kotlin LSP or kotlin-language-server itself (the `kotlin_lsp` or `kotlin_language_server` configurations of nvim-lspconfig, the Kotlin extensions of VS Code and Zed without the [Kargo extension](#zed)), give it the project description with `kargo ide` instead of switching to `kargo lsp`:

```sh
kargo ide           # writes workspace.json, which the Kotlin LSP imports
kargo ide --kls     # writes kls-classpath, which kotlin-language-server runs
```

Then open the project root in the editor. The Kotlin LSP finds `workspace.json` there (nvim-lspconfig uses it as a root marker for `kotlin_lsp`), and kotlin-language-server runs `kls-classpath` to get the classpath. Run `kargo fetch --sources` once to navigate into library sources.

You don't need to run `kargo ide` again: once the file exists, `kargo build`, `check`, `test`, `fetch`, `add`, `remove`, `update`, `upgrade` and `target` write it again when `kargo.toml` or `kargo.lock` changed. So after `kargo add`, the new library resolves once the server reloads: the Kotlin LSP reloads `workspace.json` when it changes, and kotlin-language-server reads `kls-classpath` again when you restart it.

:::note
Kargo remembers what it wrote in `target/lsp`, which `kargo clean` deletes. Run `kargo ide` after a clean to resume the updates. If it says the file was not written by Kargo, delete the file and run `kargo ide` again.
:::

:::caution
If the project folder also has an IDE's `.idea/`, a Kotlin LSP you run yourself imports that instead of `workspace.json` and your libraries don't resolve. Set its `buildTools` initialization option for the project folder to `json` (`{"buildTools": {"file:///path/to/project": "json"}}`), or move `.idea/` away. `kargo lsp` does this for you.
:::

For any other tool, [`kargo classpath`](../../reference/commands/#kargo-classpath) prints a target's classpath:

```sh
kargo classpath                          # compile classpath of the JVM target, joined with : or ;
kargo classpath --scope test --format lines
kargo classpath -t wasm --format json    # the Wasm target's klibs
```

### Runnables

The `kargo/runnables` request lists what can be run in the project, with the command line for each. Editor plugins use it for Run, Test and Debug buttons.

```json title="Request"
{ "jsonrpc": "2.0", "id": 7, "method": "kargo/runnables", "params": { "textDocument": { "uri": "file:///home/me/app/src/Main.kt" } } }
```

`textDocument` is optional. With it, only the runnables of the package containing that file are listed; without it, those of every workspace member. Each entry looks like this:

```json
{
  "label": "Run app (jvm)",
  "kind": "run",
  "package": "app",
  "target": "jvm",
  "cwd": "/home/me/app",
  "program": "/home/me/.cargo/bin/kargo",
  "args": ["run", "-t", "jvm"],
  "debugArgs": ["run", "-t", "jvm", "--debug"],
  "mainClass": "com.example.app.MainKt",
  "entryPoint": null
}
```

| Field | Meaning |
|---|---|
| `label` | A name to show |
| `kind` | `run` (a program), `test` (a target's tests), `preview` (a Compose [`@Preview`](../compose/#previews), run with `kargo preview`) or `task` (a [task](../tasks-and-plugins/)) |
| `package`, `target` | The package, and the target of `run -t` and `test -t` entries (`null` otherwise) |
| `cwd`, `program`, `args` | Run `program` with `args` in `cwd`. `program` is the `kargo` running the server |
| `debugArgs` | JVM programs and tests only: the same command with [`--debug`](#debugging), which waits for a debugger on port 5005 |
| `mainClass`, `entryPoint` | The JVM main class or Kotlin/Native entry function, when known |
| `path` | The source file of a `[[bin]]` with a `path`, of an example, or of a preview |
| `line` | A preview's 1-based line |
| `description` | A task's `description` |

Programs are listed per target (`run -t <target>`), then each `[[bin]]` (`run --bin <name>`) and example (`run --example <name>`); tests per target (`test -t <target>`, append a filter to run one test); and tasks as `task <name>`. Kargo advertises `"experimental": { "kargo": { "runnables": true } }` in its capabilities so plugins can check for the request.

### Test tree

The `kargo/testTree` request lists the test classes of the project and the test functions in them, with the `kargo test` command line of each, so an editor without a test plugin can show a test tree. Kargo's own Kotlin server answers it; it advertises `"experimental": { "kargo": { "testTree": true } }`.

```json title="Request"
{ "jsonrpc": "2.0", "id": 8, "method": "kargo/testTree", "params": { "textDocument": { "uri": "file:///home/me/app/tests/MathTest.kt" } } }
```

`textDocument` is optional: with it, only the tests of that file are listed; without it, those of every test file of the project (of the first workspace folder, when the editor opened several). The answer has one entry per file with tests:

```json
[
  {
    "uri": "file:///home/me/app/tests/MathTest.kt",
    "target": null,
    "tests": [
      {
        "id": "app.MathTest",
        "label": "MathTest",
        "kind": "class",
        "range": { "start": { "line": 4, "character": 0 }, "end": { "line": 9, "character": 1 } },
        "cwd": "/home/me/app",
        "program": "/home/me/.cargo/bin/kargo",
        "args": ["test", "app.MathTest."],
        "debugArgs": ["test", "app.MathTest.", "--debug"],
        "children": [
          {
            "id": "app.MathTest.adds",
            "label": "adds",
            "kind": "test",
            "range": { "start": { "line": 5, "character": 4 }, "end": { "line": 6, "character": 18 } },
            "cwd": "/home/me/app",
            "program": "/home/me/.cargo/bin/kargo",
            "args": ["test", "app.MathTest.adds"],
            "debugArgs": ["test", "app.MathTest.adds", "--debug"]
          }
        ]
      }
    ]
  }
]
```

A test function is a function annotated with `@Test` (kotlin.test or JUnit), `@ParameterizedTest`, `@RepeatedTest`, `@TestFactory`, `@TestTemplate` or `@Property`; a class is listed when it or a nested class has one, and abstract classes are left out. `target` is the target of a `tests-<target>/` file, and its entries run `kargo test -t <target>`. `args` end with the [filter](../testing/) that selects the class or the test; `debugArgs`, for JVM tests, add [`--debug`](#debugging).

### Status and logs

With `{ "status": true }` in the editor's `initializationOptions`, `kargo lsp` sends a `kargo/status` notification when a background job starts and when it ends, for a status bar:

```json
{ "jsonrpc": "2.0", "method": "kargo/status", "params": { "task": "checking", "active": false, "message": "2 errors" } }
```

| `task` | What runs |
|---|---|
| `indexing` | Kargo's own Kotlin server reads the project's sources and libraries |
| `resolving` | The project is imported again after `kargo.toml` or `kargo.lock` changed, which resolves its dependencies |
| `checking` | [Check on save](#check-on-save) runs `kargo check` |

`active` is `true` when the job starts and `false` when it ends; an ending `resolving` or `checking` has a `message` (`done`, `failed`, or the number of errors). A `checking` job that waits for the build lock another Kargo process holds sends `active: true` again with a `message` such as ``waiting for file lock on build directory, held by `kargo build` (process 1234)``, so the editor can show why diagnostics are late. When the editor closes the stream or sends `exit`, `kargo lsp` stops a running `kargo check` before it exits.

To see what the editor asks and how long each answer takes, start the server with a log file:

```sh
kargo lsp --log-file /tmp/kargo-lsp.log
```

Each request gets a line when it is answered, with its method, id and time, and each notification from the editor a line when it arrives:

```
[    0.412s] initialize 1 answered in 388.0 ms
[    0.415s] initialized
[    2.130s] textDocument/completion 7 answered in 12.6 ms
```

### Kotlin scripts

A `.kts` file outside a project, or one whose shebang runs `kargo script`, is a [Kotlin script](../scripts/). For those, `kargo lsp` completes the `@file:` annotations and their values, shows what each annotation means on hover, and reports a header that `kargo script` would reject as a diagnostic on that annotation. The rest of the file goes to the Kotlin server, as for any other Kotlin file, and the libraries of its `@file:DependsOn` declarations are loaded for that script alone. See [Kotlin scripts](../scripts/#editors).

## Completion and validation in `kargo.toml`

Kargo publishes a [JSON Schema](https://json-schema.org/) for `kargo.toml` at:

```
https://enekocm7.github.io/kargo/schemas/kargo.json
```

Editors that understand JSON Schemas for TOML complete keys and values, show what each key means on hover, and flag unknown keys, wrong types and invalid values (such as `environment = "nodejs"`) as you type. The schema covers package manifests and workspace manifests.

The simplest way to use it works in every editor that runs [Taplo](https://taplo.tamasfe.dev/), the TOML language server behind Even Better TOML (VS Code), Zed and Helix: add a `#:schema` comment as the first line of `kargo.toml`.

```toml title="kargo.toml"
#:schema https://enekocm7.github.io/kargo/schemas/kargo.json
[package]
name = "my-app"
version = "0.1.0"
```

To apply it to every `kargo.toml` without editing them:

- **Taplo** (any editor): add a rule to `.taplo.toml` in the project or your home directory.

  ```toml title=".taplo.toml"
  [[rule]]
  include = ["**/kargo.toml"]

  [rule.schema]
  path = "https://enekocm7.github.io/kargo/schemas/kargo.json"
  ```

- **VS Code** with Even Better TOML: add an association to your settings.

  ```json title="settings.json"
  {
    "evenBetterToml.schema.associations": {
      ".*/kargo\\.toml$": "https://enekocm7.github.io/kargo/schemas/kargo.json"
    }
  }
  ```

- **IntelliJ IDEA** and Android Studio: the [Kargo plugin](#intellij-idea-and-android-studio) gives you the `kargo.toml` support through `kargo lsp`. Without it, with the TOML plugin: in *Settings → Languages & Frameworks → Schemas and DTDs → JSON Schema Mappings*, add a mapping with the schema URL and the file pattern `kargo.toml`.

:::note
The schema checks the shape of `kargo.toml`. Some rules need Kargo itself, such as a `path` dependency's directory existing or `min-sdk <= target-sdk`. `kargo check` still reports those.
:::

## Dependency completion and update hints

`kargo lsp` also knows about the libraries you depend on. In any dependency table (`[dependencies]`, `[dev-dependencies]`, `[target.<target>.dependencies]` and `[workspace.dependencies]`):

- **Coordinates**: start typing a key such as `"io.ktor:ktor-server-c` and Kargo offers the matching artifacts from the [Maven Central search](https://central.sonatype.com/search), with the artifact names that start with what you typed first. Choosing one writes `"io.ktor:ktor-server-core" = `, ready for a version. Without a `:`, the words you typed are searched. The same works in a `coords = "..."` value.
- **Versions**: in a dependency's version (`"g:a" = "|"`, `version = "|"`, or the `version` next to `coords`), Kargo offers every version the project's [`[repositories]`](../../reference/manifest/#repositories) publish, newest stable version first and pre-releases after them.
- **npm packages**: in `[target.wasm.npm-dependencies]`, `[target.js.npm-dependencies]` and their `dev-npm-dependencies`, a package name you type (`"left-pa`) is completed from the registry's search (`/-/v1/search`; a registry without it offers none), and a package's version (`left-pad = "|"`) from the versions the registry lists, as caret ranges (`^1.3.0`), newest release first. The registry is the one `kargo add --npm` uses. Packages with a newer version get the same update hints as Maven dependencies, as `kargo outdated` reports them.
- **Update hints**: a dependency with a newer version gets an information diagnostic on its key, such as ``"`io.ktor:ktor-server-core` 3.2.3 is available (latest: 4.0.0)"``. The rules are those of [`kargo outdated`](../../reference/commands/#kargo-outdated): the newest compatible version (same major, or same minor for `0.x`), and the latest one when it differs. Ranges and BOM-managed versions get no hint. Run `kargo upgrade` to apply them.

```toml title="kargo.toml"
[dependencies]
"io.ktor:ktor-server-core" = "3.1.0"   # ← `io.ktor:ktor-server-core` 3.2.3 is available
"io.ktor:ktor-server-c|"               # ← offers ktor-server-cio, ktor-server-core, ...
```

All of this runs in the background: Kotlin traffic and `kargo.toml` errors never wait for the network. Searches wait for a pause in your typing, and their results are kept in `~/.kargo/cache/maven-search` for a day. Versions come from the `maven-metadata.xml` files Kargo caches for [dependency resolution](../dependencies/). Hints are worked out when you open `kargo.toml` and after each [re-import](#language-server), not on every keystroke.

With `kargo --offline lsp`, coordinates aren't searched, and versions and hints come from what is already cached and from repositories on disk. Set `KARGO_MAVEN_SEARCH_URL` to search a mirror of the Maven Central search API instead (see [Environment](../../reference/environment/)).

## Code actions in `kargo.toml`

`kargo lsp` fixes and edits `kargo.toml` for you through your editor's code actions (the light bulb, or *Quick Fix* / *Source Action*). Each action makes the same change as the Kargo command it stands for, with the same format-preserving editor, so comments and layout stay as you wrote them.

**Quick fixes**, offered on the line of an error or hint:

| On | Fix |
|---|---|
| An unknown key with a "did you mean" | Rename it to the suggestion: `mian-class` becomes `main-class` |
| A value that isn't allowed | Change it to each allowed value, the closest one first: `environment = "browsr"` becomes `"browser"` |
| A missing required key | Add it under the table's header: `version = "0.1.0"` in `[package]`, a `name` in `[[bin]]` (taken from its `path`), or a whole `[package]` table |
| An [update hint](#dependency-completion-and-update-hints) | Upgrade the dependency to the newest compatible version, as `kargo upgrade` does, or to the latest one, as `kargo upgrade --incompatible` does |
| A line holding only `"group:artifact"` in a dependency table | Add it at its latest version, as `kargo add group:artifact` does |
| A line holding only a package name in an npm table (`[target.wasm.npm-dependencies]`) | Add it at its latest version, as `kargo add --npm <name>` does |

```toml title="kargo.toml"
[target.wasm]
environment = "browsr"          # ← Change to `browser`

[dependencies]
"io.ktor:ktor-server-core"      # ← Add `io.ktor:ktor-server-core` at its latest version
```

**Adding frameworks and targets**, offered anywhere in a package's `kargo.toml` once it parses:

- *Enable the `<name>` framework* for each [built-in framework](../frameworks/) that isn't enabled, writing `<name> = true` under `[frameworks]` as `kargo add --framework <name>` does. Ktor isn't offered, since it needs an engine: write `ktor = { server = "netty" }` yourself.
- *Add the `<target>` target* for each target that isn't enabled. It runs `kargo target add <target>`: it writes `[target.<target>]` with the defaults `kargo new` uses (and `[target.jvm]` first, if `kargo.toml` had no `[target]` section) and scaffolds the target's files, such as `src-native/` or the Android manifest.

Inside `[frameworks]` or a `[target.*]` table they are refactorings, so they show in the light bulb; elsewhere they are source actions, which editors list under *Source Action* instead.

Adding a dependency picks its version from your [`[repositories]`](../../reference/manifest/#repositories), so it runs in the background like completion. Edits to a `kargo.toml` open in the editor arrive as a `workspace/applyEdit`, so you can undo them and save when you like; a `kargo.toml` that isn't open is written directly.

### Commands for editor plugins

Actions that need the network or write files are LSP commands, which plugins can also run directly with `workspace/executeCommand`, for example after asking for coordinates in an input box:

| Command | Arguments | Does |
|---|---|---|
| `kargo/addDependency` | `{ "uri", "coords", "section"?, "line"? }` | `kargo add`: `coords` is one coordinate (optionally with `:version`) or a list. `section` is `dependencies` (the default), `dev-dependencies` or `target.<target>.dependencies`. `line`, zero-based, is a half-typed dependency line to replace. Replies with `{ "added": ["group:artifact:version"], "section" }` |
| `kargo/addTarget` | `{ "uri", "target" }` | `kargo target add <target>`. Replies with `{ "targets", "jvmMaterialized", "created" }`, `created` listing the scaffolded files relative to the package |

```json title="Request"
{ "jsonrpc": "2.0", "id": 9, "method": "workspace/executeCommand", "params": { "command": "kargo/addDependency", "arguments": [{ "uri": "file:///home/me/app/kargo.toml", "coords": "io.ktor:ktor-server-core" }] } }
```

`uri` is the package's `kargo.toml`. Kargo lists both commands in `executeCommandProvider`. A command fails with an error reply, leaving `kargo.toml` alone, when the result would not be a valid manifest, when `kargo.toml` changed while the version was being looked up, or when the document is open in an editor that doesn't support `workspace/applyEdit`.

## Importing a project

`kargo metadata` prints the whole project as JSON: every package with its source, test and resource roots, output directories, compiler arguments and plugins, entry points and tasks, and the resolved classpath (JVM) or klibs (Wasm, Native) of every target. Editor plugins and language servers read it instead of `kargo.toml`, so they analyze the code with the same libraries and compiler flags the build uses.

```sh
kargo metadata              # resolves dependencies, like kargo tree
kargo metadata --no-deps    # roots and settings only; downloads nothing
```

The roots include the output directories of [`[[generate]]` steps](../code-generation/) (`target/generated/<profile>/<name>/`), whether or not a build has filled them yet. `kargo metadata` never runs a step itself; `kargo generate` does.

To keep an import current, watch the files listed in `watch` (every `kargo.toml` and `kargo.lock`). When one changes, run `kargo metadata --fingerprint`. If it prints something other than the `fingerprint` you imported, import again.

The format is described field by field in [Machine-readable output](../../reference/machine-readable-output/#kargo-metadata).

## Library sources

Navigating into a library or stepping into it in a debugger needs its sources. Download the `-sources.jar` of every dependency once:

```sh
kargo fetch --sources
```

`kargo metadata` then lists each library's sources jar next to its jar, in the `sources` field. For a library that doesn't publish sources, its Javadoc jar is downloaded instead and listed in the `javadoc` field, so [Kargo's own Kotlin server](../kotlin-server/#documentation-from-javadoc-jars) can still show its documentation. [Kargo's own Kotlin server](../kotlin-server/#libraries) downloads them itself unless `fetchSources` is `false` in the editor's `initializationOptions`.

## Check on save

[Kargo's own Kotlin server](../kotlin-server/) reports only syntax problems, unused code and deprecated usages as you type, so with it check on save is on: the errors come from `kargo check`, which compiles your code for real. The Kotlin LSP and kotlin-language-server analyze your code for the JVM, with the compiler plugins and flags they know about, so errors only the real build finds don't show until you run `kargo build`: type errors in `src-wasm/` or `src-native/`, errors from compiler plugins, and [`warnings-as-errors`](../../reference/manifest/#kotlin) failures. With check on save, `kargo lsp` finds them for you. Turn it on (or off, with `false`) in the editor's `initializationOptions`:

```lua title="init.lua"
vim.lsp.config('kargo', {
  cmd = { 'kargo', 'lsp' },
  filetypes = { 'kotlin', 'toml' },
  root_markers = { 'kargo.lock', 'kargo.toml' },
  init_options = { checkOnSave = true },
})
```

```toml title="~/.config/helix/languages.toml"
[language-server.kargo]
command = "kargo"
args = ["lsp"]
config = { checkOnSave = true }
```

:::note
Compile errors (type errors, unresolved names) appear only after a save. Opening or editing a file reports none of them. The bundled extensions and the usual editors send `textDocument/didSave` themselves, but a client you drive by hand or write yourself must send it, or `kargo lsp` reports no compile errors. The check also waits for the build lock, so it can take a while when a build is running.
:::

When you save a `.kt` or `.java` file, Kargo runs `kargo check --tests --keep-going --message-format json` for the package that contains it (every member, when the nearest `kargo.toml` is a workspace root) and shows the result:

- Each error and warning is a diagnostic with source `kargo` and the targets that report it as its code, for example `kargo(native, wasm)`. Messages without a file, such as the `-Werror` summary, go on `kargo.toml`.
- Diagnostics the Kotlin server already reports (same line, same message) are left out, so a JVM error shows once.
- The next check replaces them: fix the error, save, and it goes away.
- The check is reported with `$/progress`. Saving again while a check runs stops it and starts a new one.
- If the check fails before compiling (a dependency that can't be resolved, a broken `kargo.toml`), the diagnostics stay as they were and the reason goes to the language server log.

| Option | Default | Meaning |
|---|---|---|
| `checkOnSave` | `true` with Kargo's own server, else `false` | Run `kargo check` when a Kotlin or Java file is saved |
| `checkTargets` | every enabled target | The targets to check, for example `["wasm", "native"]` to leave the JVM to the Kotlin server |

Kargo merges its diagnostics into the Kotlin server's whether that server pushes them (`textDocument/publishDiagnostics`, kotlin-language-server) or the editor pulls them (`textDocument/diagnostic`, the Kotlin LSP). With pulled diagnostics, Kargo asks the editor to pull again with `workspace/diagnostic/refresh` after a check, and pushes the diagnostics of files that aren't open. Android sources are checked through Gradle, whose errors are not converted, so they show only in the log.

:::note
A check takes the same lock on `target/` as any other build, so a `kargo build` in a terminal waits for it, and the other way round.
:::

## Compiler errors in other tools

For check-on-save in an editor or tool that doesn't use `kargo lsp`, run [`kargo check`](../../reference/commands/#kargo-check-c), which type-checks without packaging or linking, and ask for JSON:

```sh
kargo check --message-format json
```

Each error and warning is one JSON line with the file, line, column, severity and message, ending with a `build-finished` line. See [Compiler diagnostics](../../reference/machine-readable-output/#compiler-diagnostics). Pass `--tests` to check the tests too.

## Test explorer

A test explorer needs the tests before they run and their results while they run. Kargo gives both as JSON lines on stdout:

```sh
kargo test --list --format json       # compile and list the tests, without running them
kargo test --message-format json      # run them, reporting each one as it starts and ends
```

`--list` prints a `test-discovered` message per test with an `id`, its class (`suite`), `name`, `display-name`, and the `file` and `line` that declare it, for gutter icons and "go to test". A run prints `test-started` and `test-finished` messages with the same ids, then a `test-summary` per target. `test-finished` carries the outcome, the duration, the failure message and stack trace, and what the test printed. To run one test, pass its `Class.name` as the filter: `kargo test --message-format json com.example.app.MathTest.adds`.

JVM and Native tests can be listed; Wasm and Android tests report results but can't be listed yet. The fields are described in [Test discovery](../../reference/machine-readable-output/#test-discovery) and [Test results](../../reference/machine-readable-output/#test-results). The [Kargo extension for VS Code](#visual-studio-code) and the [JetBrains plugin](#intellij-idea-and-android-studio) build their test explorers on these messages.

## Debugging

`--debug` starts the JVM suspended, listening for a debugger on port 5005 of `127.0.0.1`. The program, or the tests, start once a debugger attaches.

```console
$ kargo run --debug
   Compiling my-app v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.84s
note: the program waits for a debugger: attach one over JDWP to 127.0.0.1 on port 5005
     Running `java com.example.app.MainKt`
Listening for transport dt_socket at address: 5005
```

Pick another port with `--debug=8000`, or let the JVM choose one with `--debug=0` (it prints the port). `kargo test --debug` does the same for the JVM tests; see [Debugging tests](../testing/#debugging-tests). `--debug` works for the JVM target only.

Attach with any JDWP debugger:

- **IntelliJ IDEA**: with the [Kargo plugin](#run-test-and-debug), *Debug* a Kargo run configuration and the debugger attaches by itself. Without it: *Run → Edit Configurations → + → Remote JVM Debug*, host `localhost`, port `5005`. Start `kargo run --debug`, then debug that configuration.
- **VS Code**: the [Kargo extension](#visual-studio-code) debugs through `kargo dap`, with a Debug button above each `fun main` and test. Without it, with the Debugger for Java extension, add an attach configuration:

  ```json title=".vscode/launch.json"
  {
    "version": "0.2.0",
    "configurations": [
      { "type": "java", "name": "Attach to kargo", "request": "attach", "hostName": "localhost", "port": 5005 }
    ]
  }
  ```

- **jdb**: `jdb -attach 127.0.0.1:5005`.
- **Zed**: the [Kargo extension](#debug-in-zed) debugs through `kargo dap`, below.

### Debug adapter

`kargo dap` is a debug adapter: editors with a [Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/) client start it, and it starts the program for them. It runs fwcd's [kotlin-debug-adapter](https://github.com/fwcd/kotlin-debug-adapter), which Kargo downloads on first use, on the project's JDK.

A `launch` request names a Kargo command line in `args`. Kargo builds and runs it with `--debug`, shows what the program prints in the debug console, and attaches when the JVM is ready, so a breakpoint on the first line of `main` is hit. An `attach` request attaches to a program you started with `kargo run --debug`:

```json title="Launch"
{ "request": "launch", "args": ["run", "--bin", "server"] }
```

```json title="Attach"
{ "request": "attach", "port": 5005 }
```

Tests are debugged the same way, with `["test", "com.example.app.MathTest.adds"]`. The arguments are listed in [`kargo dap`](../../reference/commands/#kargo-dap).

kotlin-debug-adapter expects Gradle's directory layout to find classes and source files. Kargo makes your layout fit: it rewrites the source paths between the editor and the adapter, so breakpoints in `src/`, `src-jvm/` and `tests/` resolve, and stack frames open your files. A breakpoint resolves in the top-level functions of its file, in the classes, objects and interfaces the file declares at the top level, and in the class an `@file:JvmName` names.

**Neovim** with [nvim-dap](https://github.com/mfussenegger/nvim-dap) is set up by the [Neovim plugin](#debugging-with-nvim-dap), or by hand with the same `kargo dap` adapter.

**Emacs** with [Dape](https://github.com/svaante/dape) is set up by the [Emacs package](#debugging-with-dape).

**Helix**:

```toml title="~/.config/helix/languages.toml"
[[language]]
name = "kotlin"
debugger = { name = "kargo", transport = "stdio", command = "kargo", args = ["dap"], templates = [
  { name = "kargo run", request = "launch", completion = [], args = { args = ["run"] } },
] }
```

Start the editor in the project, or in a directory inside it: `kargo dap` debugs the project of its working directory.

:::note
Debugging works for JVM programs and tests. kotlin-debug-adapter doesn't support conditional breakpoints or logpoints, and evaluates variable names only, not arbitrary expressions.
:::

Breakpoints in library code resolve once [library sources](#library-sources) are downloaded.
