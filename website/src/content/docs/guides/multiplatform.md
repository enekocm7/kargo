---
title: Multiplatform projects
description: Share Kotlin code between the JVM, WebAssembly, JavaScript and Kotlin/Native with expect/actual.
---

A Kargo project can target several platforms at once: `jvm`, `wasm`, `js`, `native` and `android`. Code in `src/` is compiled for all of them; code in `src-<target>/` only for one.

## Create a multiplatform project

```sh
kargo new hello -t jvm,wasm,native
```

```toml title="kargo.toml"
[package]
name = "hello"
version = "0.1.0"
group = "com.example"

[target.jvm]

[target.wasm]
environment = "js"

[target.native]
platforms = ["host"]
kind = "executable"
```

To add a target to an existing project, use `kargo target add`:

```sh
kargo target add wasm native
```

It appends the same `[target.<name>]` sections `kargo new` writes, without touching the rest of `kargo.toml`, and creates an empty `src-<target>/` for platform-specific code. `kargo target remove wasm` disables a target again (its sources stay on disk), and `kargo target list` shows what is enabled. See [`kargo target`](../../reference/commands/#kargo-target).

:::note
A project with **no** `[target]` sections is a JVM project. As soon as you add any `[target.*]` section, only the listed targets are built. `kargo target add` handles this for you by also writing `[target.jvm]`; if you edit `kargo.toml` by hand, add it yourself.
:::

## Sharing code with `expect`/`actual`

Declare what you need in common code with `expect`, then provide an `actual` for every target:

```kotlin title="src/Platform.kt"
package com.example.hello

expect fun platformName(): String

fun greeting(): String = "Hello from ${platformName()}!"

fun main() {
    println(greeting())
}
```

```kotlin title="src-jvm/Platform.kt"
package com.example.hello

actual fun platformName(): String = "the JVM (Java ${System.getProperty("java.version")})"
```

```kotlin title="src-wasm/Platform.kt"
package com.example.hello

actual fun platformName(): String = "WebAssembly"
```

```kotlin title="src-native/Platform.kt"
package com.example.hello

import kotlin.experimental.ExperimentalNativeApi

@OptIn(ExperimentalNativeApi::class)
actual fun platformName(): String = "Kotlin/Native on ${Platform.osFamily}"
```

## Build, run and test each target

```console
$ kargo build                 # all targets
$ kargo run                   # the first of: jvm, native, wasm, js, android
Hello from the JVM (Java 21.0.5)!
$ kargo run -t native
Hello from Kotlin/Native on LINUX!
$ kargo run -t wasm
Hello from WebAssembly!
$ kargo test                  # tests on every target
$ kargo test -t native
$ kargo test --no-fail-fast   # keep testing the other targets when one fails
```

`kargo test` stops at the first target that fails; `--no-fail-fast` (or `[test] fail-fast = false`) tests them all and prints a result per target. `kargo build --keep-going` builds every target and reports each failure. See [Testing every target](../testing/#testing-every-target).

When the host can't build every target, or you rarely want to, list the ones `build`, `check` and `test` use without `-t`. The manifest keeps all its targets, and `-t` still picks any of them:

```toml title="kargo.toml"
[build]
default-targets = ["jvm", "wasm"]
```

In a workspace, write it as `[workspace.build]`. See [`[build]`](../../reference/manifest/#build).

:::tip
`environment = "js"` runs the Wasm target on Node.js. For a web front end use `environment = "browser"`: `kargo run -t wasm` then serves a page instead, and `kargo test -t wasm` runs the tests in a headless browser. See [WebAssembly](../wasm/#browser-apps).
:::

## JavaScript next to WebAssembly

`[target.js]` compiles the same common code to JavaScript with Kotlin/JS, for consumers that can't use Wasm. Its own code goes in `src-js/` and `tests-js/`, and its own dependencies in `[target.js.dependencies]`:

```toml title="kargo.toml"
[target.jvm]

[target.js]
environment = "node"
```

```kotlin title="src-js/Platform.kt"
package com.example.hello

actual fun platformName(): String = "Kotlin/JS"
```

```console
$ kargo run -t js
Hello from Kotlin/JS!
```

Wasm and JS are separate targets in Kargo, as in Gradle (`wasmJs()` and `js()`), so a project that enables both writes an `actual` for each. See [JavaScript](../js/).

## Dependencies

Multiplatform libraries go in `[dependencies]`; Kargo picks the right artifact for each target using Gradle module metadata. Platform-specific libraries go in `[target.<name>.dependencies]`:

```toml title="kargo.toml"
[dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
"com.squareup.okio:okio" = "3.10.2"

[target.jvm.dependencies]
"org.slf4j:slf4j-simple" = "2.0.17"
```

Using a JVM-only library in `src/` fails to compile on the other targets. That's your cue to move the code to `src-jvm/`.

## Tips

- Keep business logic in `src/` and put only the thin platform layer in `src-<target>/`.
- Prefer multiplatform libraries ([klibs.io](https://klibs.io) lists them) over `java.*` APIs in common code. For example, use `kotlinx-datetime` instead of `java.time`, and `okio` or `kotlinx-io` for files.
- Tests in `tests/` run on every target, which is a cheap way to catch platform differences.
- `kargo doc` documents `src/` as common code and shows which platforms each declaration is available on. See [API documentation](../documentation/).
- In an editor, `kargo lsp` treats `src/` and each `src-<target>/` as separate source sets, so `expect` and `actual` declarations link up and common code only sees common code and the libraries every target has. [Kargo's own Kotlin server](../kotlin-server/#how-it-reads-your-project) reads the code of every target; with JetBrains' Kotlin LSP, common code is analyzed as JVM code and Wasm, JS and Native code gets no Kotlin code intelligence yet. See [Multiplatform projects](../editors/#multiplatform-projects).
- `kargo publish` publishes a multiplatform library the way the Kotlin Gradle Plugin does, with one module per platform, so Gradle and Kargo projects pick the right artifact. See [Publishing a library](../publishing/#multiplatform-libraries).
