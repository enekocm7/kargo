---
title: Editor integration
description: Run kargo lsp in your editor, get completion and validation in kargo.toml, import a project into a language server, browse library sources, drive a test explorer and attach a debugger.
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
- **Re-import** when `kargo.toml` or `kargo.lock` change: on save, or when the editor reports a changed file (Kargo asks to be told about every `kargo.toml` and `kargo.lock`). If the project's [fingerprint](#importing-a-project) changed, Kargo resolves the dependencies again in the background, reports it with `$/progress`, and the Kotlin server reloads. Add a dependency with `kargo add` and its classes complete a moment later.
- **Runnables**: the [`kargo/runnables`](#runnables) request lists programs, tests and tasks with the command lines that run them, for Run and Debug buttons.

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

kotlin-language-server is no longer maintained and its last release bundles Kotlin 2.1, so it understands code and libraries built with newer Kotlin versions only partly. Prefer the default server.

Kargo pins the version of each server and installs it like any other [toolchain](../toolchains/#kotlin-language-servers). `kargo toolchain install kotlin-lsp` downloads it ahead of time; `kargo --offline lsp` uses what is installed and the dependencies already in the cache.

### Files it writes

Kotlin language servers don't read `kargo.toml`, so Kargo writes the project description each one understands at the project root, from [`kargo metadata`](#importing-a-project):

| Server | File | Contents |
|---|---|---|
| Kotlin LSP | `workspace.json` | A module per package for main code and one for tests, with their source and resource roots, the JVM target's classpaths as libraries (with sources jars once downloaded with `kargo fetch --sources`), path dependencies as modules, the JDK, `jvm-target`, compiler arguments and compiler plugins |
| kotlin-language-server | `kls-classpath` (`kls-classpath.bat` on Windows) | A script printing the test classpath, which Kargo keeps in `target/lsp/classpath.txt` |

They hold paths on your machine, so keep them out of version control; `kargo new` adds them to `.gitignore`. Kargo only replaces a file while it still holds what Kargo last wrote: a `workspace.json` you wrote yourself, or one another tool generated, is left alone, and `kargo lsp` tells you so. Delete it to let Kargo manage it. Kargo remembers the last complete import in `target/lsp`, so a restart with an unchanged `kargo.toml` and `kargo.lock` starts the Kotlin server right away.

If the dependencies can't be resolved (offline, a broken `kargo.toml`), Kargo describes the project without them and says why. It imports again once `kargo.toml` or `kargo.lock` change.

:::caution
The Kotlin LSP understands JVM code. Kargo describes each package's JVM target to it, so code that only exists for Wasm, Native or Android (in `src-wasm/`, `src-native/` or `android/`) gets no Kotlin code intelligence yet. `kargo.toml` support and runnables work for every target.
:::

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

## Compiler errors in the editor

For check-on-save, run [`kargo check`](../../reference/commands/#kargo-check-c), which type-checks without packaging or linking, and ask for JSON:

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
