---
title: JavaScript (Kotlin/JS)
description: Compile Kotlin to JavaScript with Kotlin/JS for Node.js and browsers, run and test it, use npm packages, and publish npm and Maven libraries.
---

Kargo compiles Kotlin to JavaScript with the Kotlin/JS compiler (the IR backend, `js(IR)` in Gradle). Use it when the code has to run where WebAssembly doesn't: older browsers, Node.js tools, npm packages that plain JavaScript projects consume, and multiplatform libraries whose users declare `js()`. No Gradle and no webpack are involved; Kargo only runs npm, pnpm or bun when the project needs [npm packages](#npm-packages).

For new browser apps, [WebAssembly](../wasm/) is usually faster. The two targets are separate: a project can enable both, with `src-wasm/` and `src-js/` for their own code.

## Setup

```toml title="kargo.toml"
[target.js]
environment = "node"   # "node" (default) or "browser"
```

| Environment | Use it for | Libraries Kargo adds |
|---|---|---|
| `node` (default) | Programs and libraries that run on Node.js. `kargo run` and `kargo test` run them with `node`. | `kotlin-stdlib-js`; `kotlin-test-js` for tests |
| `browser` | Web pages. Kargo adds an HTML page, serves it with `kargo run` and runs the tests in a headless browser. | The same, and `kotlinx-browser` (`document`, `window`, `org.w3c.dom`) |

`kargo new hello -t js` creates a project with this target, and `kargo target add js` adds it to an existing one. See [`kargo target`](../../reference/commands/#kargo-target).

## Run and test with Node.js

You need [Node.js](https://nodejs.org) on your `PATH` for `kargo run -t js` and `kargo test -t js`, but not for building.

```console
$ kargo new hello -t js
$ cd hello
$ kargo run -- a b
   Compiling hello v0.1.0 (js)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.50s
     Running `node target/debug/js/hello.mjs a b`
Hello, Kotlin!
```

Arguments after `--` reach `main(args)`, as on the JVM: Kargo has the compiler pass `process.argv` after the script path to it. Build output goes to `target/<profile>/js/`.

```console
$ kargo test
   Compiling hello v0.1.0 (js, tests)
     Running Kotlin/JS tests (Node.js)
test com.example.hello.GreetingTest.greetsByName ... ok (0ms)

test result: ok. 1 passed; 0 failed; 0 ignored; finished in 510ms
```

Tests use `kotlin.test` as on every other target. Kargo runs them with a small runner of its own, so there is no Mocha or Karma to install: the runner collects the tests kotlin-test registers when the test module loads, runs them one after the other (waiting for the ones that return a `Promise`, such as `runTest` from kotlinx-coroutines-test) and reports them to Kargo. What the tests print, failures with their stack traces, `@Ignore`, a test name filter (`kargo test GreetingTest`), `--report junit` and `--message-format json` all work. Dev builds have source maps and Kargo starts Node.js with `--enable-source-maps`, so stack traces point at your Kotlin files.

## Browser apps

```toml title="kargo.toml"
[target.js]
environment = "browser"
```

```kotlin title="src/Main.kt"
package com.example.hello

import kotlinx.browser.document

fun main() {
    document.body?.append("Hello from Kotlin/JS")
}
```

```console
$ kargo run -t js
   Compiling hello v0.1.0 (js)
     Serving target/debug/js at http://127.0.0.1:8080/ (press Ctrl+C to stop)
```

`kargo run` serves the app the way it serves a [Wasm browser app](../wasm/#browser-apps), with the same `--port` and `--open` options, and the same `port` key in `[target.js]` to change the default port 8080 (see [Choosing the port](../wasm/#choosing-the-port)). Kargo writes `index.html` next to the module, unless `resources/index.html` is your own page; everything below `resources/` is copied to `target/<profile>/js/`. The generated page loads `<name>.mjs` with `<script type="module">`, or `<name>.js` with a plain `<script>` for a UMD module.

`kargo run --watch` rebuilds the app on each change and reloads the open pages, see [Reload a browser app on change](../watch/#reload-a-browser-app-on-change).

`kargo test` runs the tests in a headless browser, found and configured as for [Wasm](../wasm/#testing-in-a-browser) (`KARGO_BROWSER`, `KARGO_BROWSER_ARGS`, `KARGO_BROWSER_TIMEOUT`). Any current browser runs Kotlin/JS.

## Module kinds

The compiler writes an ES module by default. Set `module-kind` for other consumers:

```toml title="kargo.toml"
[target.js]
module-kind = "commonjs"   # "es" (default), "commonjs" or "umd"
```

| `module-kind` | Files | Loaded with |
|---|---|---|
| `es` (default) | `<name>.mjs` | `import`, `<script type="module">` |
| `commonjs` | `<name>.js` | `require` on Node.js |
| `umd` | `<name>.js` | `require`, AMD, or a plain `<script>` tag, which puts the module on `globalThis` |

A browser can't load a CommonJS module, so a `browser` application needs `es` or `umd`. Tests always run as an ES module, whatever the program is. `module-name` changes `<name>` (default: the package name).

## Release builds

`kargo build --release` links with the compiler's dead code elimination (`-Xir-dce`): only what `main` and `@JsExport` reach stays, and no source map is written. For a library with one exported function that takes the module from about 560 KiB to under 1 KiB. Dev builds keep everything and write `<name>.mjs.map` with the Kotlin sources embedded.

## Libraries and npm packages

Set `kind = "library"` and mark what JavaScript and TypeScript may use with `@JsExport`:

```toml title="kargo.toml"
[package]
name = "greeter"
version = "0.3.0"
license = "MIT"

[target.js]
kind = "library"
npm-name = "@acme/greeter"   # default: the lowercase package name
```

```kotlin title="src/Greeter.kt"
package com.example.greeter

@OptIn(ExperimentalJsExport::class)
@JsExport
fun greet(name: String): String = "Hello, $name!"
```

```console
$ kargo build --release
   Compiling greeter v0.3.0 (js)
$ ls target/release/js
greeter.d.mts  greeter.mjs  package.json
```

`target/<profile>/js/` is an npm package: publish it with `npm publish target/release/js`.

- **Declarations.** `<name>.d.mts` (`<name>.d.ts` for CommonJS and UMD) holds the TypeScript declarations of the `@JsExport` declarations (`export declare function greet(name: string): string;`). Unlike Wasm, Kotlin/JS exports classes and objects too. An ES module exports them by name (`import { greet } from "@acme/greeter"`); CommonJS and UMD modules put them in objects named after their Kotlin package (`require("@acme/greeter").com.example.greeter.greet`).
- **`package.json`** comes from `[package]` (`name`, `version`, `description`, `license`, `author`), with `type` (`module`, or `commonjs` for the other kinds), `main`, `types`, `exports`, `files`, and the [npm packages](#npm-packages) the library needs as `dependencies`.
- A library has no `main`, so `kargo run` refuses to run it; `kargo test` runs its tests.

To publish a Kotlin library for other Kotlin projects, use [`kargo publish`](../publishing/): it publishes the klib as the `<name>-js` module, with the Gradle module metadata (`org.jetbrains.kotlin.platform.type = js`, `org.jetbrains.kotlin.js.compiler = ir`) that Gradle and Kargo builds select it by.

## npm packages

Kotlin/JS libraries that import npm packages at runtime list them in a `package.json` inside their klib. Kargo installs them after compiling, with the package manager rules described for [Wasm](../wasm/#npm-packages): the first of npm, pnpm and bun on your `PATH` (or `package-manager`), lifecycle scripts off (`--ignore-scripts`), the highest version when two libraries disagree, and `--offline` passed on.

The packages go to `target/<profile>/js/node_modules`, next to the module, where Node.js finds them. When you remove the last npm dependency, the next build deletes that `node_modules` and the `package.json` Kargo wrote. For a browser app, the page gets an import map for them and `kargo run` serves them at `/node_modules/` (an ES module app only; a UMD page can't use an import map).

For an npm package of your own, list it in `[target.js.npm-dependencies]`. Kargo generates Kotlin declarations for it from its TypeScript declarations, as it does [for Wasm](../wasm/#kotlin-declarations-for-npm-packages), so you call it like a Kotlin library:

```toml title="kargo.toml"
[target.js.npm-dependencies]
left-pad = "1.3.0"
```

`kargo add --npm left-pad -t js` writes the entry for you (`-t js` is only needed when the project has the wasm target too), and `kargo remove --npm`, `kargo outdated` and `kargo upgrade` work on it; see [Your own npm packages](../wasm/#your-own-npm-packages). `[target.js.dev-npm-dependencies]` holds the packages only `kargo test` installs.

```kotlin title="src/Main.kt"
import left_pad.leftPad

fun main() = println("[" + leftPad("7", 3, "0") + "]")
```

```console
$ kargo run
  Installing npm packages left-pad@1.3.0 (npm)
  Generating Kotlin declarations for npm packages left-pad
   Compiling user v0.1.0 (js)
     Running `node target/debug/js/user.mjs`
[007]
```

The declarations go to `target/<profile>/generated/npm-js/` (`@types` packages to `target/<profile>/npm-types-js/`), with the same Kotlin packages, names and [type mapping](../wasm/#kotlin-declarations-for-npm-packages) as on Wasm: they use the interop types `JsAny`, `JsString`, `JsArray` and `Promise`, which Kotlin/JS shares with Kotlin/Wasm since Kotlin 2.2.20, so code that calls an npm package compiles for both targets. Kargo passes the opt-in `kotlin.js.ExperimentalWasmJsInterop` they need. With an older Kotlin, Kargo warns and generates none. Options objects are built with the same [factories](../wasm/#building-objects) (`make(Options(size = 2))`), copied to `target/<profile>/js/js/kargo-factories/`; they are ES modules, so use `module-kind = "es"` (the default) for a program that calls them.

A package without TypeScript declarations gets none, with a warning: write the `external` declarations you need with `@JsModule`, or use `dynamic`. The Kotlin docs cover [JavaScript interop](https://kotlinlang.org/docs/js-interop.html) and [`@JsModule`](https://kotlinlang.org/docs/js-modules.html).

## Your own JavaScript and TypeScript

JavaScript and TypeScript modules in `js/` work as they do [on Wasm](../wasm/#your-own-javascript-and-typescript): Kargo copies the JavaScript, compiles the TypeScript with tsc (the newest release, or [`[toolchain] typescript`](../../reference/manifest/#toolchain)) into `target/<profile>/js/js/`, and generates Kotlin declarations for them into `target/<profile>/generated/js-sources-js/`, so `js/greeter.ts` is the Kotlin package `js.greeter`:

```ts title="js/greeter.ts"
export function greet(name: string): string {
    return `Hello, ${name}!`;
}
```

```kotlin title="src/Main.kt"
import js.greeter.greet

fun main() = println(greet("Kargo"))
```

Set `js-sources` in `[target.js]` to use other directories. The TypeScript is type-checked against the module's own `<name>.d.mts`, so it can import the `@JsExport` declarations of your Kotlin code (`import { exclaim } from "../app.mjs"`). With both targets enabled, Wasm and JS share `js/` and each gets its own copy of the modules and declarations. The modules are ES modules; use `module-kind = "es"` (the default) for a program that imports them.

## Multiplatform

`src/` is common code for every enabled target and `src-js/` the JS target's own, with `tests-js/` for its tests. `expect` declarations in `src/` get their `actual` in `src-js/`:

```toml title="kargo.toml"
[target.jvm]
[target.js]
```

```kotlin title="src-js/Platform.kt"
package com.example.mpp

actual fun platform() = "js"
```

Dependencies only the JS target uses go in `[target.js.dependencies]`. Kargo resolves every dependency to its Kotlin/JS variant from Gradle module metadata, the same way Gradle does, so a library has to publish a `js` variant built with the IR compiler: JVM-only libraries and libraries built only for the legacy Kotlin/JS compiler can't be used. The lockfile keeps the resolutions under `js/main` and `js/test`. See [Multiplatform](../multiplatform/).

## Frameworks

`[frameworks]` serialization, coroutines and the Ktor client (`ktor = { client = "js" }`) work on the JS target. Compose for Web runs on Kotlin/Wasm; use [`[target.wasm]`](../compose/#compose-for-the-web) for it. The Ktor server, Lombok and Spring Boot need the JVM. See [Frameworks](../frameworks/).

## Output files

| File | What it is |
|---|---|
| `<name>.mjs` or `<name>.js` | The program or library, with the standard library and the other klibs linked in |
| `<name>.mjs.map` | The source map (dev builds) |
| `<name>.d.mts` or `<name>.d.ts` | TypeScript declarations of the `@JsExport` declarations |
| `package.json` | The npm package (libraries only) |
| `js/` | Your own [JavaScript and TypeScript modules](#your-own-javascript-and-typescript), TypeScript compiled |
| `index.html` and your `resources/` | The page of a `browser` app |
| `node_modules/` | The npm packages the module imports |
| `test/` | The tests and their runner, after `kargo test` |

## Limitations

- `kargo test --list` doesn't list JS tests yet.
- The whole program is linked into one file; there is no per-module output or incremental linking yet.
- `kargo run` serves a browser app as it is; it doesn't rebuild or reload the page when the sources change.
