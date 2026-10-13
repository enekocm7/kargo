---
title: Zed
description: Install the Kargo extension for Zed to get Kotlin code intelligence, kargo.toml support, run buttons, tasks and debugging in Kargo projects.
---

The Kargo extension makes [Zed](https://zed.dev) understand Kargo projects. It runs [`kargo lsp`](../editors/#language-server) for Kotlin files and `kargo.toml`, so completion, navigation and errors use your dependencies, and it debugs programs and tests through [`kargo dap`](../editors/#debug-adapter). With the tasks `kargo ide --zed` writes, the run buttons next to `fun main` and your tests run Kargo.

Open a folder that contains a `kargo.toml` and the extension takes over. You don't need a `build.gradle.kts` or a `pom.xml`.

## Install

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

## Code intelligence

When you open a Kotlin file or `kargo.toml` in a folder with a `kargo.toml` at its root, Zed starts `kargo lsp` there. You get everything listed in [Editor integration](../editors/#language-server):

- Completion, go to definition (into library sources once you run `kargo fetch --sources`), find references, hover, rename, quick fixes and formatting in Kotlin files, from JetBrains' Kotlin LSP. Kargo describes your project to it from `kargo.toml`, and again when `kargo.toml` or `kargo.lock` change.
- Completion, documentation, validation and quick fixes in `kargo.toml`, with Maven coordinate and version completion and hints at newer versions. The quick fixes add dependencies, frameworks and targets the way `kargo add` and `kargo target add` do.
- [Check on save](../editors/#check-on-save): when you save a Kotlin file, the errors of `kargo check` for every target appear in the editor and in *diagnostics: deploy*. The extension turns it on.

The first start downloads the Kotlin LSP and resolves your dependencies, so it takes a while.

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

### Settings

`kargo lsp` reads its options from `lsp.kargo`:

```json title="settings.json"
{
  "lsp": {
    "kargo": {
      "binary": { "arguments": ["--server", "kls"] },
      "initialization_options": { "checkOnSave": true, "checkTargets": ["wasm", "native"] }
    }
  }
}
```

| Setting | Default | Meaning |
|---|---|---|
| `binary.path` | the `kargo` on `PATH` | The `kargo` to run |
| `binary.arguments` | none | Added after `kargo lsp`: [`--server kls`, `--server none`, `--klib-targets`](../editors/#choosing-the-kotlin-server) |
| `binary.env` | none | Environment variables for `kargo lsp` |
| `initialization_options.checkOnSave` | `true` | Run `kargo check` when a Kotlin or Java file is saved |
| `initialization_options.checkTargets` | every enabled target | The targets check on save compiles |

## Run and test

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

*task: spawn* lists the other tasks: each target's program and tests (`kargo run -t wasm`, `kargo test -t native`), each [task](../tasks-and-plugins/) in `kargo.toml` (`kargo task <name>`), and `kargo build`, `build --release`, `check --tests`, `test`, `clean`, `fetch --sources`, `update` and `outdated`. Select `group:artifact` coordinates in an editor and run `kargo add $ZED_SELECTED_TEXT` to add them as a dependency, or `kargo remove $ZED_SELECTED_TEXT` to remove one. Bind any task to a key with the `task::Spawn` action and its `task_name`.

Kargo keeps the file current: after you change `kargo.toml`, `kargo lsp` and commands such as `kargo build` and `kargo add` write it again, so a new `[[bin]]`, target or task gets its task. Kargo never replaces a `.zed/tasks.json` it didn't write, so to keep your own tasks next to Kargo's, put yours in Zed's global tasks (*zed: open tasks*). If you edit the generated file, Kargo stops updating it; delete it and run `kargo ide --zed` to get it back.

Compiler errors in a task's output name the file, line and column; Cmd-click (Ctrl-click on Linux and Windows) one in the terminal to open it.

## Debug

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

`args` are the arguments of `kargo`; Kargo adds `--debug`, and `-t jvm` unless they name a target. Breakpoints, stepping, the call stack and variables work; the debug console shows what the program prints. The options are listed in [`kargo dap`](../../reference/commands/#kargo-dap), and what Kargo does behind them in [Debug adapter](../editors/#debug-adapter).

`kargo dap` downloads kotlin-debug-adapter on first use and runs it on the project's JDK.

## Assistant context

Type `/kargo-metadata` in a text thread of the assistant panel to insert the project as Kargo sees it: each package's targets with their source and test directories, its programs, examples and tasks, and its `kargo.toml` with the dependencies and frameworks. The assistant then knows where code goes and which libraries it can use.

## `kargo.toml`

`kargo lsp` validates `kargo.toml` against Kargo's [JSON Schema](../editors/#completion-and-validation-in-kargotoml) and Kargo's own rules, so you don't need anything else. If you also use Zed's TOML language server (Taplo), add `#:schema https://enekocm7.github.io/kargo/schemas/kargo.json` as the first line of `kargo.toml` for Taplo's completion too, or limit `kargo.toml` to Kargo's server.

## Limitations

- Zed has no test explorer: run tests from their buttons or tasks, and read the results in the terminal. A failed test is re-run from its button.
- The extension starts `kargo lsp` only when `kargo.toml` is at the root of the folder you open. Open the project root, not a directory above it.
- Code in `src-wasm/`, `src-native/` and `android/` gets no Kotlin code intelligence from the Kotlin LSP yet. Check on save still reports its errors.
- Debugging works for the JVM target only. kotlin-debug-adapter doesn't support conditional breakpoints or logpoints, and evaluates variable names only.
- Slash commands work in text threads; the agent panel's threads don't run them.
