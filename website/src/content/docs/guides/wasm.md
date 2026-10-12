---
title: WebAssembly
description: Compile Kotlin to WebAssembly for Node.js, browsers or WASI.
---

Kargo compiles Kotlin to WebAssembly with the Kotlin/Wasm compiler. No Gradle and no npm project are involved; Kargo only runs npm, pnpm or bun when a library needs [npm packages](#npm-packages).

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

## npm packages

Some Kotlin/Wasm libraries import npm packages at runtime. For example, `kotlinx-datetime` uses `@js-joda/core`. These libraries list their npm packages in a `package.json` inside the published klib, and Kargo installs them for you after compiling:

```console
$ kargo add org.jetbrains.kotlinx:kotlinx-datetime:0.7.1
$ kargo run -t wasm
   Compiling hello v0.1.0 (wasm)
  Installing npm packages @js-joda/core@3.2.0 (npm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 17.38s
     Running `node target/debug/wasm/hello.mjs`
```

- Kargo writes `target/<profile>/package.json` (`target/<profile>/<member>/package.json` in a [workspace](../workspaces/)) and runs the package manager's `install` there, so `node_modules` sits next to the `wasm/` directory where Node.js finds it. The install runs again only when the set of packages changes.
- When two libraries ask for different versions of the same package, the highest version wins.
- npm packages that only your `[dev-dependencies]` need are installed into `target/<profile>/wasm/test/` for `kargo test`.
- This needs a package manager on your `PATH`, even for `kargo build`. Projects whose libraries need no npm packages never run one.
- With `--offline`, Kargo passes `--offline` to the package manager, which then installs only from its own cache.

### Choosing npm, pnpm or bun

By default Kargo uses the first of `npm`, `pnpm` and `bun` it finds on your `PATH`. To pick one, set `package-manager`:

```toml title="kargo.toml"
[target.wasm]
package-manager = "bun"   # "npm", "pnpm" or "bun"
```

The package manager's lockfile (`package-lock.json`, `pnpm-lock.yaml` or `bun.lock`) stays in `target/<profile>/`. When you switch package managers, Kargo deletes the old `node_modules` and lockfile and installs again. The packages always run on Node.js, whichever tool installed them.

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
- npm packages are installed for Node.js only. If you load the output in a browser, bundle the packages listed in `target/<profile>/package.json` yourself.
