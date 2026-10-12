---
title: Editor integration
description: Get completion and validation in kargo.toml, import a project into an editor or language server, browse library sources and attach a debugger.
---

Kargo gives editors what they need to understand a project without reading `kargo.toml` themselves: a JSON Schema for `kargo.toml`, a JSON description of the project, library sources, machine-readable compiler errors and a debugger port.

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
