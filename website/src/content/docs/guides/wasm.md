---
title: WebAssembly
description: Compile Kotlin to WebAssembly for Node.js, browsers or WASI.
---

Kargo compiles Kotlin to WebAssembly with the Kotlin/Wasm compiler. No Gradle and no npm project are involved.

## Setup

```toml title="kargo.toml"
[target.wasm]
environment = "js"     # or "wasi"
```

| Environment | Use it for | Standard library |
|---|---|---|
| `js` (default) | Node.js and browsers. Can call JavaScript through `external` declarations and `js(...)`. | `kotlin-stdlib-wasm-js` |
| `wasi` | Standalone runtimes using the WebAssembly System Interface. | `kotlin-stdlib-wasm-wasi` |

## Run and test

[Node.js](https://nodejs.org) runs Wasm programs and tests. You need it on your `PATH` for `kargo run -t wasm` and `kargo test -t wasm`, but not for building.

```console
$ kargo new hello -t wasm
$ cd hello
$ kargo run
   Compiling hello v0.1.0 (wasm)
     Running `node target/debug/wasm/hello.mjs`
Hello, Kotlin!
```

Build output goes to `target/<profile>/wasm/`. It contains the `.wasm` module and the JavaScript glue that loads it.

## Calling JavaScript (environment `js`)

```kotlin title="src-wasm/Console.kt"
@file:OptIn(ExperimentalWasmJsInterop::class)

package com.example.hello

// Bind to an existing JavaScript function.
@JsFun("(message) => console.warn(message)")
external fun warn(message: String)

fun shout(text: String) = warn(text.uppercase())
```

The Kotlin docs cover [JavaScript interop for Wasm](https://kotlinlang.org/docs/wasm-js-interop.html) in depth.

## Using it from a web page

`kargo build --release -t wasm` produces an ES module you can import from your own JavaScript or bundler. Serve the `target/release/wasm/` directory and load the `.mjs` file with `<script type="module">`.

## Limitations

- **Compose for Web is not supported.** It needs the Compose Gradle plugin's resource pipeline. Use the JVM (desktop) or Android target for Compose.
- Libraries must publish a Wasm variant (`wasmJs` or `wasmWasi`). JVM-only libraries can't be used.
- Test name filters are ignored on Wasm; all tests run.
