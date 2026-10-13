---
title: LSP project model
description: The project description kargo lsp gives Kargo's own Kotlin server, for authors of language servers and other tools.
---

`kargo lsp --server kargo` describes the project to [Kargo's Kotlin server](../../guides/editors/#kargos-kotlin-server-preview) with one JSON document, the project model. It is built from [`kargo metadata`](../machine-readable-output/#kargo-metadata) and shaped for a language server: modules with their source roots, dependencies and compiler settings, and libraries listed once. Nothing is written into the project.

This page is for people who write a Kotlin language server or another tool that sits behind `kargo lsp`. Editor users don't need it.

## Where it comes from

| Message | Direction | Contents |
|---|---|---|
| `initialize` | `kargo lsp` → server | The model in `initializationOptions.kargo.project`, next to the editor's own options |
| `kargo/projectChanged` | `kargo lsp` → server | After each import (when `kargo.toml` or `kargo.lock` changed): `{ "project": <model>, "changedModules": [<ids>] }`. `changedModules` lists the modules that were added, removed or changed; the model is always complete |
| `kargo/status` | server → `kargo lsp` | `{ "state": "indexing" \| "ready" \| "degraded", "message": "..." }`. `kargo lsp` shows `indexing` as `$/progress` until `ready`, and logs the reason of `degraded` |

`kargo lsp` also forwards `workspace/didChangeWatchedFiles` for `.kt`, `.java` and `.klib` files to the server; it registers the watchers itself.

## Example

```json
{
  "modelVersion": 1,
  "kargoVersion": "0.1.0",
  "root": "file:///home/me/app",
  "targetDir": "/home/me/app/target",
  "resolved": true,
  "jdk": { "home": "/home/me/.kargo/toolchains/jdk/21.0.8", "version": 21 },
  "kotlin": { "projectVersion": "2.4.21" },
  "libraries": [
    {
      "id": "org.jetbrains.kotlin:kotlin-stdlib:2.4.21",
      "kind": "jar",
      "path": "/home/me/.kargo/cache/maven/org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21.jar",
      "sources": null,
      "sha256": "5f1c...",
      "platform": "jvm"
    }
  ],
  "modules": [
    {
      "id": "app.jvm.main",
      "package": "app",
      "member": true,
      "sourceSet": "jvm",
      "scope": "main",
      "platform": ["jvm"],
      "sourceRoots": ["/home/me/app/src", "/home/me/app/src-jvm"],
      "resourceRoots": ["/home/me/app/resources"],
      "generatedRoots": [],
      "dependsOn": [],
      "friends": [],
      "dependencies": ["lib:org.jetbrains.kotlin:kotlin-stdlib:2.4.21"],
      "compiler": {
        "languageVersion": null,
        "apiVersion": null,
        "optIn": [],
        "warningsAsErrors": false,
        "multiplatform": false,
        "jvmTarget": "21",
        "freeArgs": [],
        "plugins": []
      }
    },
    {
      "id": "app.jvm.test",
      "scope": "test",
      "friends": ["app.jvm.main"],
      "dependencies": ["module:app.jvm.main", "lib:org.jetbrains.kotlin:kotlin-stdlib:2.4.21", "..."],
      "...": "..."
    }
  ]
}
```

## Versioning

`modelVersion` is `1`. It changes only when a field is removed or changes its meaning; new fields can appear in any release, so ignore the ones you don't know. A server should refuse a model version it doesn't read with a message that asks to update Kargo and the server together.

## Modules

There is one module per package, source set and scope. Its `id` is `<package>.<sourceSet>.<scope>`.

| Package | Source sets |
|---|---|
| One target | One source set named after the target (`jvm`, `wasm`, `native`), with `src/` and `src-<target>/` (tests: `tests/` and `tests-<target>/`) |
| Several targets | `common` for `src/` and `tests/`, compiled for every target, and one per target for `src-<target>/` and `tests-<target>/`, which `dependsOn` the common one |

Android source sets are left out; Android Studio covers them.

| Field | Meaning |
|---|---|
| `id` | `<package>.<sourceSet>.<scope>`, unique in the model |
| `package`, `member` | The package's name, and whether it is a workspace member rather than a package reached through a path dependency |
| `sourceSet` | `common`, `jvm`, `wasm` or `native` |
| `scope` | `main` or `test` |
| `platform` | What the code is analyzed for: `jvm`, `wasm-js`, `wasm-wasi` or `native:<platform>` (a Native source set is analyzed for the first of its platforms). A common source set lists every platform of its package |
| `sourceRoots`, `resourceRoots` | Directories, whether they exist or not. Resources belong to JVM source sets |
| `generatedRoots` | Directories of generated sources; always empty for now |
| `dependsOn` | The common source set this one refines: its `expect` declarations are implemented here |
| `friends` | Modules whose `internal` declarations this one sees: a test module's main modules |
| `dependencies` | In order: `module:<id>` for modules (the main module of a test module, path dependencies and workspace members, as source) and `lib:<id>` for libraries. Only what the code compiles against: the compile classpath for main code, the test classpath for tests |
| `compiler` | The settings below |

A common source set gets the libraries of its package's JVM target, or of its first target without one, since Kargo resolves libraries per platform rather than their common variants. Its path dependencies are the common modules of multiplatform packages.

| `compiler` field | Meaning |
|---|---|
| `languageVersion`, `apiVersion` | From `[kotlin]`, or `null` for the compiler's default |
| `optIn` | `[kotlin] opt-in` |
| `warningsAsErrors` | `[kotlin] warnings-as-errors` |
| `multiplatform` | Whether the module belongs to a package with several targets, which allows `expect` and `actual` |
| `jvmTarget` | The bytecode version of a JVM source set, else `null` |
| `freeArgs` | The other Kotlin compiler arguments of the target: profile settings and the frameworks' flags |
| `plugins` | Each compiler plugin: `id`, its Maven `module`, and its `options` as `{ "name", "value" }` pairs. A server maps the `id` to an implementation built for its own frontend |

## Libraries

| Field | Meaning |
|---|---|
| `id` | `group:artifact:version`, with the file name added when one module has several files |
| `kind` | `jar`, `klib`, or the file's extension |
| `path` | The file, or the directory of a Kotlin/Native distribution klib |
| `sources` | The sources jar if it is in Kargo's cache ([`kargo fetch --sources`](../commands/#kargo-fetch)), else `null` |
| `sha256` | The checksum `kargo.lock` pins, else `null`. Use it to key caches and indexes that outlive a project |
| `platform` | `jvm`, `wasm-js`, `wasm-wasi`, `native:<platform>`, or `native` for the Kotlin/Native standard library |

Native source sets also get the standard and platform libraries of the installed Kotlin/Native distribution, with ids such as `kotlin-native:2.4.21:stdlib` and `kotlin-native:2.4.21:linux_x64:posix`.

## The project

| Field | Meaning |
|---|---|
| `root` | The project root as a `file:` URI |
| `targetDir` | The build directory. Kargo's server logs to `<targetDir>/lsp/kotlin-server.log` |
| `resolved` | `false` when the dependencies couldn't be resolved (offline, a broken `kargo.toml`): modules then have no libraries, and Kargo sends a complete model once they resolve |
| `jdk` | The project's JDK, `home` and major `version`, or `null` when there is none |
| `kotlin.projectVersion` | The project's `[kotlin] version` |

Paths are absolute, and Windows drive letters are upper-case (`C:\...`), so a server can compare them with paths from `file:` URIs after normalizing those the same way.
