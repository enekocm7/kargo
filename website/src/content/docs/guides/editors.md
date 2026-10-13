---
title: Editor integration
description: Run kargo lsp in your editor, get completion, validation and quick fixes in kargo.toml, import a project into a language server, browse library sources, drive a test explorer and attach a debugger.
---

Kargo gives editors what they need to understand a project without reading `kargo.toml` themselves: a language server, a JSON Schema for `kargo.toml`, a JSON description of the project, library sources, machine-readable compiler errors, test discovery and per-test results, and a debugger port.

## Language server

`kargo lsp` is one language server for a whole Kargo project. Point your editor at it for Kotlin files and `kargo.toml`:

```sh
kargo lsp
```

It speaks LSP over stdin and stdout and gives you:

- **Kotlin code intelligence** (completion, go to definition, references, hover, rename, quick fixes, diagnostics) from [JetBrains' Kotlin LSP](https://github.com/Kotlin/kotlin-lsp). Kargo downloads the server on first use, describes your project to it, and passes Kotlin traffic through unchanged.
- **`kargo.toml` support**: completion of keys, tables and values, documentation on hover, and errors as you type. Errors come from the [JSON Schema](#completion-and-validation-in-kargotoml) first (unknown keys with a "did you mean", wrong types, invalid values), then from Kargo's own validation and [warnings](../../reference/warnings/), the same messages `kargo build` prints.
- **Dependencies in `kargo.toml`**: completion of `group:artifact` coordinates from Maven Central and of versions from your repositories, and a hint on every dependency with a newer version. See [Dependency completion and update hints](#dependency-completion-and-update-hints).
- **Code actions in `kargo.toml`**: quick fixes for its errors, and the edits of `kargo add`, `kargo add --framework`, `kargo target add` and `kargo upgrade` from the editor. See [Code actions in `kargo.toml`](#code-actions-in-kargotoml).
- **Re-import** when `kargo.toml` or `kargo.lock` change: on save, or when the editor reports a changed file (Kargo asks to be told about every `kargo.toml` and `kargo.lock`). If the project's [fingerprint](#importing-a-project) changed, Kargo resolves the dependencies again in the background, reports it with `$/progress`, and the Kotlin server reloads. Add a dependency with `kargo add` and its classes complete a moment later.
- **Runnables**: the [`kargo/runnables`](#runnables) request lists programs, tests and tasks with the command lines that run them, for Run and Debug buttons.
- **Check on save** (opt-in): when you save a Kotlin file, `kargo check` runs in the background and its errors for every target (Wasm and Native included) appear next to the Kotlin server's. See [Check on save](#check-on-save).

The first start downloads the Kotlin LSP (about 360 MB) and resolves the project's dependencies, so it takes a while. Later starts reuse both.

### Editor setup

**Neovim** (0.11 or newer):

```lua title="init.lua"
vim.lsp.config('kargo', {
  cmd = { 'kargo', 'lsp' },
  filetypes = { 'kotlin', 'toml' },
  root_markers = { 'kargo.lock', 'kargo.toml' },
})
vim.lsp.enable('kargo')
```

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

**Emacs** with Eglot:

```elisp title="init.el"
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs
               '((kotlin-mode kotlin-ts-mode conf-toml-mode toml-ts-mode) . ("kargo" "lsp"))))
```

Any other editor with a generic LSP client works the same way: run `kargo lsp` for Kotlin and TOML files. Kargo answers for every TOML file it is sent but only gives `kargo.toml` features, so other TOML files are unaffected.

:::note
Open the project root or any directory in it. Kargo finds the project from the folder the editor opens and points the Kotlin server at the project root, so opening a workspace member works too.
:::

### Choosing the Kotlin server

| Command | Kotlin server |
|---|---|
| `kargo lsp` | JetBrains' Kotlin LSP (the default) |
| `kargo lsp --server kls` | fwcd's [kotlin-language-server](https://github.com/fwcd/kotlin-language-server), for setups that still use it. It runs on the project's JDK, or on a Temurin 21 Kargo installs when that is newer than 21 |
| `kargo lsp --server none` | No Kotlin server: only `kargo.toml` support and runnables |
| `kargo lsp --server-path <path>` | The server executable at `<path>` instead of the one Kargo downloads |
| `kargo lsp --klib-targets` | Also describe Wasm and Native code to the Kotlin LSP. See [Multiplatform projects](#multiplatform-projects) |

kotlin-language-server is no longer maintained and its last release bundles Kotlin 2.1, so it understands code and libraries built with newer Kotlin versions only partly. Prefer the default server.

Kargo pins the version of each server and installs it like any other [toolchain](../toolchains/#kotlin-language-servers). `kargo toolchain install kotlin-lsp` downloads it ahead of time; `kargo --offline lsp` uses what is installed and the dependencies already in the cache.

### Files it writes

Kotlin language servers don't read `kargo.toml`, so Kargo writes the project description each one understands at the project root, from [`kargo metadata`](#importing-a-project):

| Server | File | Contents |
|---|---|---|
| Kotlin LSP | `workspace.json` | A module per package for main code and one for tests (a module per source set for a [multiplatform](#multiplatform-projects) package), with their source and resource roots, the JVM target's classpaths as libraries (with sources jars once downloaded with `kargo fetch --sources`), path dependencies as modules, the JDK, `jvm-target`, compiler arguments and compiler plugins |
| kotlin-language-server | `kls-classpath` (`kls-classpath.bat` on Windows) | A script printing the test classpath, which Kargo keeps in `target/lsp/classpath.txt` |

[`kargo ide`](#without-kargo-lsp) writes the same files without running a server. They hold paths on your machine, so keep them out of version control; `kargo new` adds them to `.gitignore`. Kargo only replaces a file while it still holds what Kargo last wrote: a `workspace.json` you wrote yourself, or one another tool generated, is left alone, and `kargo lsp` tells you so. Delete it to let Kargo manage it. Kargo remembers the last complete import in `target/lsp`, so a restart with an unchanged `kargo.toml` and `kargo.lock` starts the Kotlin server right away.

If the dependencies can't be resolved (offline, a broken `kargo.toml`), Kargo describes the project without them and says why. It imports again once `kargo.toml` or `kargo.lock` change.

### Multiplatform projects

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
Code that only exists for Wasm, Native or Android (in `src-wasm/`, `src-native/` or `android/`) gets no Kotlin code intelligence from the Kotlin LSP yet. Use Android Studio for Android code. `kargo.toml` support, runnables and check on save work for every target.
:::

### Without `kargo lsp`

If your editor already runs JetBrains' Kotlin LSP or kotlin-language-server itself (the `kotlin_lsp` or `kotlin_language_server` configurations of nvim-lspconfig, the Kotlin extensions of VS Code and Zed), give it the project description with `kargo ide` instead of switching to `kargo lsp`:

```sh
kargo ide           # writes workspace.json, which the Kotlin LSP imports
kargo ide --kls     # writes kls-classpath, which kotlin-language-server runs
```

Then open the project root in the editor. The Kotlin LSP finds `workspace.json` there (nvim-lspconfig uses it as a root marker for `kotlin_lsp`), and kotlin-language-server runs `kls-classpath` to get the classpath. Run `kargo fetch --sources` once to navigate into library sources.

You don't need to run `kargo ide` again: once the file exists, `kargo build`, `check`, `test`, `fetch`, `add`, `remove`, `update`, `upgrade` and `target` write it again when `kargo.toml` or `kargo.lock` changed. So after `kargo add`, the new library resolves once the server reloads: the Kotlin LSP reloads `workspace.json` when it changes, and kotlin-language-server reads `kls-classpath` again when you restart it.

:::note
Kargo remembers what it wrote in `target/lsp`, which `kargo clean` deletes. Run `kargo ide` after a clean to resume the updates. If it says the file was not written by Kargo, delete the file and run `kargo ide` again.
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
| `kind` | `run` (a program), `test` (a target's tests) or `task` (a [task](../tasks-and-plugins/)) |
| `package`, `target` | The package, and the target of `run -t` and `test -t` entries (`null` otherwise) |
| `cwd`, `program`, `args` | Run `program` with `args` in `cwd`. `program` is the `kargo` running the server |
| `debugArgs` | JVM programs and tests only: the same command with [`--debug`](#debugging), which waits for a debugger on port 5005 |
| `mainClass`, `entryPoint` | The JVM main class or Kotlin/Native entry function, when known |
| `path` | The source file of a `[[bin]]` with a `path`, or of an example |
| `description` | A task's `description` |

Programs are listed per target (`run -t <target>`), then each `[[bin]]` (`run --bin <name>`) and example (`run --example <name>`); tests per target (`test -t <target>`, append a filter to run one test); and tasks as `task <name>`. Kargo advertises `"experimental": { "kargo": { "runnables": true } }` in its capabilities so plugins can check for the request.

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

- **IntelliJ IDEA** and Android Studio (with the TOML plugin): in *Settings → Languages & Frameworks → Schemas and DTDs → JSON Schema Mappings*, add a mapping with the schema URL and the file pattern `kargo.toml`.

:::note
The schema checks the shape of `kargo.toml`. Some rules need Kargo itself, such as a `path` dependency's directory existing or `min-sdk <= target-sdk`. `kargo check` still reports those.
:::

## Dependency completion and update hints

`kargo lsp` also knows about the libraries you depend on. In any dependency table (`[dependencies]`, `[dev-dependencies]`, `[target.<target>.dependencies]` and `[workspace.dependencies]`):

- **Coordinates**: start typing a key such as `"io.ktor:ktor-server-c` and Kargo offers the matching artifacts from the [Maven Central search](https://central.sonatype.com/search), with the artifact names that start with what you typed first. Choosing one writes `"io.ktor:ktor-server-core" = `, ready for a version. Without a `:`, the words you typed are searched. The same works in a `coords = "..."` value.
- **Versions**: in a dependency's version (`"g:a" = "|"`, `version = "|"`, or the `version` next to `coords`), Kargo offers every version the project's [`[repositories]`](../../reference/manifest/#repositories) publish, newest stable version first and pre-releases after them.
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

To keep an import current, watch the files listed in `watch` (every `kargo.toml` and `kargo.lock`). When one changes, run `kargo metadata --fingerprint`. If it prints something other than the `fingerprint` you imported, import again.

The format is described field by field in [Machine-readable output](../../reference/machine-readable-output/#kargo-metadata).

## Library sources

Navigating into a library or stepping into it in a debugger needs its sources. Download the `-sources.jar` of every dependency once:

```sh
kargo fetch --sources
```

`kargo metadata` then lists each library's sources jar next to its jar, in the `sources` field. Libraries that don't publish sources are skipped.

## Check on save

The Kotlin server analyzes your code for the JVM, with the compiler plugins and flags it knows about. Errors only the real build finds don't show until you run `kargo build`: type errors in `src-wasm/` or `src-native/`, errors from compiler plugins, and [`warnings-as-errors`](../../reference/manifest/#kotlin) failures. With check on save, `kargo lsp` finds them for you. Turn it on in the editor's `initializationOptions`:

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

When you save a `.kt` or `.java` file, Kargo runs `kargo check --tests --keep-going --message-format json` for the package that contains it (every member, when the nearest `kargo.toml` is a workspace root) and shows the result:

- Each error and warning is a diagnostic with source `kargo` and the targets that report it as its code, for example `kargo(native, wasm)`. Messages without a file, such as the `-Werror` summary, go on `kargo.toml`.
- Diagnostics the Kotlin server already reports (same line, same message) are left out, so a JVM error shows once.
- The next check replaces them: fix the error, save, and it goes away.
- The check is reported with `$/progress`. Saving again while a check runs stops it and starts a new one.
- If the check fails before compiling (a dependency that can't be resolved, a broken `kargo.toml`), the diagnostics stay as they were and the reason goes to the language server log.

| Option | Default | Meaning |
|---|---|---|
| `checkOnSave` | `false` | Run `kargo check` when a Kotlin or Java file is saved |
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

JVM and Native tests can be listed; Wasm and Android tests report results but can't be listed yet. The fields are described in [Test discovery](../../reference/machine-readable-output/#test-discovery) and [Test results](../../reference/machine-readable-output/#test-results).

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

- **IntelliJ IDEA**: *Run → Edit Configurations → + → Remote JVM Debug*, host `localhost`, port `5005`. Start `kargo run --debug`, then debug that configuration.
- **VS Code** with the Debugger for Java extension: add an attach configuration.

  ```json title=".vscode/launch.json"
  {
    "version": "0.2.0",
    "configurations": [
      { "type": "java", "name": "Attach to kargo", "request": "attach", "hostName": "localhost", "port": 5005 }
    ]
  }
  ```

- **jdb**: `jdb -attach 127.0.0.1:5005`.

Breakpoints in library code resolve once [library sources](#library-sources) are downloaded.
