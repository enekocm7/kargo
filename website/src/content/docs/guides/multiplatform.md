---
title: Multiplatform projects
description: Share Kotlin code between the JVM, WebAssembly and Kotlin/Native with expect/actual.
---

A Kargo project can target several platforms at once. Code in `src/` is compiled for all of them; code in `src-<target>/` only for one.

## Create a multiplatform project

```sh
kargo new hello -t jvm,wasm,native
```

```toml title="kargo.toml"
[package]
name = "hello"
version = "0.1.0"
group = "com.example"

[kotlin]
version = "2.4.21"

[target.jvm]
jvm-target = "17"

[target.wasm]
environment = "js"

[target.native]
platforms = ["host"]
kind = "executable"
```

Or add a target to an existing project by adding its `[target.<name>]` section.

:::note
A project with **no** `[target]` sections is a JVM project. As soon as you add any `[target.*]` section, only the listed targets are built.
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
$ kargo run                   # the first of: jvm, native, wasm, android
Hello from the JVM (Java 21.0.5)!
$ kargo run -t native
Hello from Kotlin/Native on LINUX!
$ kargo run -t wasm
Hello from WebAssembly!
$ kargo test                  # tests on every target
$ kargo test -t native
```

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
