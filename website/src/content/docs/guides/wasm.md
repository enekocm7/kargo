---
title: WebAssembly
description: Compile Kotlin to WebAssembly for Node.js, browsers or WASI, serve browser apps, and publish Wasm libraries as npm packages.
---

Kargo compiles Kotlin to WebAssembly with the Kotlin/Wasm compiler. To compile to plain JavaScript instead, with Kotlin/JS, see [JavaScript](../js/). No Gradle and no npm project are involved. Kargo only runs npm, pnpm or bun when a project needs [npm packages](#npm-packages), and installs pinned Binaryen on demand for [release builds](#release-builds).

## Setup

```toml title="kargo.toml"
[target.wasm]
environment = "browser"   # "js" (default), "browser" or "wasi"
```

| Environment | Use it for | Standard library |
|---|---|---|
| `js` (default) | Programs and libraries that run on Node.js. They can call JavaScript through `external` declarations and `js(...)`. | `kotlin-stdlib-wasm-js` |
| `browser` | Web pages. Kargo adds an HTML page, serves it with `kargo run` and runs the tests in a headless browser. See [Browser apps](#browser-apps). | `kotlin-stdlib-wasm-js` and `kotlinx-browser-wasm-js` (the browser APIs) |
| `wasi` | Standalone runtimes using the WebAssembly System Interface. | `kotlin-stdlib-wasm-wasi` |

In an existing project, `kargo target add wasm` writes `[target.wasm]` with `environment = "js"` (and keeps an implicit JVM target enabled); change the environment afterwards if you need another. See [`kargo target`](../../reference/commands/#kargo-target).

`js` and `browser` compile to the same module and use the same libraries. The environment decides what Kargo does around it: Node.js runs a `js` program, and a browser loads a `browser` app from the page Kargo serves.

## Run and test with Node.js

[Node.js](https://nodejs.org) runs `js` and `wasi` programs and tests. You need it on your `PATH` for `kargo run -t wasm` and `kargo test -t wasm`, but not for building.

```console
$ kargo new hello -t wasm
$ cd hello
$ kargo run
   Compiling hello v0.1.0 (wasm)
     Running `node target/debug/wasm/hello.mjs`
Hello, Kotlin!
```

Build output goes to `target/<profile>/wasm/`. It contains the `.wasm` module and the JavaScript glue that loads it.

### Program arguments

Arguments after `--` reach `main(args)`, as on the JVM and Native:

```kotlin title="src/Main.kt"
package com.example.hello

fun main(args: Array<String>) = println("args=" + args.toList())
```

```console
$ kargo run -- a "b c"
     Running `node target/debug/wasm/hello.mjs a b c`
args=[a, b c]
```

Kotlin/Wasm itself always calls `main` with an empty array. When your `main` takes `args`, Kargo compiles a small generated `main()` in the default package along with your sources (the compiler starts that one instead of yours), and it calls your `main` with the arguments:

- With `environment = "js"` they come from `process.argv` after the script path. Outside Node.js, for example in a browser, `args` is empty.
- With `environment = "wasi"` they come from the WASI `args_get` call, without the program name (and without the `.mjs` launcher when Node.js runs it).
- With `environment = "browser"` there is no command line, so `args` is always empty.

This works only when your `main` is in a named package, which `kargo new` always sets up. A `main(args)` in the default package gets an empty array, and Kargo warns about it when it compiles.

## Browser apps

With `environment = "browser"`, `kargo run` builds the app and serves it, and `kargo test` runs the tests in a browser.

```toml title="kargo.toml"
[target.wasm]
environment = "browser"
```

```console
$ kargo run -t wasm
   Compiling hello v0.1.0 (wasm)
     Serving target/debug/wasm at http://127.0.0.1:8080/ (press Ctrl+C to stop)
```

Open the address in a browser. The page loads your program, and whatever it prints with `println` appears in the browser's developer console. Stop the server with Ctrl+C.

| Option | Meaning |
|---|---|
| `--port <port>` | The port to listen on. The default is `8080`; `0` picks a free one. Kargo only listens on `127.0.0.1`, so the page is not reachable from other machines |
| `--open` | Open the page in your browser (the one named by `$BROWSER`, else the system default) |

Kargo serves the files with the content types browsers need (`application/wasm` for the module, JavaScript for the `.mjs` files) and without caching, so reload the page after you run `kargo run` again.

### The page

Without any setup Kargo writes `index.html` next to the module: a short page that loads `<module>.mjs` with `<script type="module">`. To build the page yourself, put your files in `resources/`. Kargo copies everything below `resources/` into `target/<profile>/wasm/`, and a `resources/index.html` replaces the generated page:

```html title="resources/index.html"
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>My app</title>
  <link rel="stylesheet" href="style.css">
</head>
<body>
  <div id="root"></div>
  <script type="module" src="./hello.mjs"></script>
</body>
</html>
```

The module is named after the package (`hello.mjs` here), or after [`module-name`](#module-name-and-output-files). The Kotlin code reaches the page through the browser APIs: the `kotlinx.browser` package (`document`, `window`, and the DOM in `org.w3c.dom`) or `external` declarations and `js(...)`. The Kotlin docs cover [JavaScript interop for Wasm](https://kotlinlang.org/docs/wasm-js-interop.html) in depth.

Kargo adds `kotlinx-browser-wasm-js` to every `browser` app and library, at the version it ships with, so `kotlinx.browser` builds without a dependency of your own and your IDE sees the same APIs:

:::note
The browser APIs are a dependency like any other, so they are recorded in `kargo.lock` and a published browser library passes them on to its users. To use another version, declare it in `[target.wasm.dependencies]`; the usual rules for conflicting versions apply, so the highest one wins unless you set `force = true`.
:::

:::note
`resources/` is a JVM concept too (it ends up in the jar). In a project that targets both, the files in `resources/` go to the jar and to the browser app.
:::

Libraries can ship files to the page too, and a script that runs before the app: see [Web resources from libraries](#web-resources-from-libraries). Your own `resources/index.html` has to import those scripts itself.

### Testing in a browser

`kargo test` serves a page that runs your tests and starts a browser without a window to load it. The tests report their results back to Kargo, which prints them like it does for any other target:

```console
$ kargo test -t wasm
   Compiling hello v0.1.0 (wasm, tests)
     Running Wasm tests (headless browser)
test com.example.hello.GreetingTest.greetsByName ... ok

test result: ok. 1 passed; 0 failed; 0 ignored
```

What the tests print, failures with their stack traces and `--report junit` all work as for Node.js. Test name filters are not supported on Wasm.

Kargo looks for a browser in this order: the one named by `KARGO_BROWSER`, then Chrome, Chromium, Edge, Brave and Firefox on your `PATH` and in their usual install locations. Each run uses a throwaway profile, so a browser you have open is left alone. Kargo treats an executable whose name contains `firefox` as Firefox and anything else as a Chromium-based browser (Chrome, Edge, Chromium, Brave), because the two take different command lines. When the tests are done Kargo stops the browser; it asks Firefox to close its window instead of ending its process, since Firefox can hand over to another process, for example when it applies an update on startup. Without a browser, `kargo test` fails and tells you to install one; `kargo test --no-run` doesn't need one.

| Variable | Meaning |
|---|---|
| `KARGO_BROWSER` | The browser to use: a path to its executable, or a command on `PATH` |
| `KARGO_BROWSER_ARGS` | Extra command-line arguments for it, separated by spaces. Chrome and the other Chromium-based browsers usually need `--no-sandbox` in a container |
| `KARGO_BROWSER_TIMEOUT` | Seconds the browser may stay silent before the tests count as hung (default `120`) |

The browser has to support WebAssembly garbage collection and exception handling, which Kotlin/Wasm requires; current Chrome, Edge and Firefox do, and so does Safari for the people who open your page. See [CI](../ci/#wasm) for running these tests on a build server.

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
- `kargo fetch` installs these packages ahead of time, when a package manager is on `PATH`. It works from the Maven dependencies, so the packages of a [path dependency](../workspaces/#path-dependencies) are installed by the first build.

### Your own npm packages

To use an npm package from your own code, list it in `[target.wasm.npm-dependencies]`. Kargo installs it with the others, and your entry wins when a library asks for another version of the same package:

```toml title="kargo.toml"
[target.wasm.npm-dependencies]
express = "5.1.0"
```

Kargo then generates Kotlin declarations for the package from its TypeScript declarations, so you call it like a Kotlin library, and your editor completes and navigates it:

```kotlin title="src/Main.kt"
import express.express

fun main() {
    val app = express()
    app.get("/") { req, res, _ -> res.status(200).send("Hello from Kotlin at ${req.path}") }
    app.listen(3000) { println("Listening on http://localhost:3000") }
}
```

```console
$ kargo run
  Installing npm packages express@5.1.0 (npm)
  Installing npm packages @types/express@^5 (npm)
  Generating Kotlin declarations for npm packages express
   Compiling web v0.1.0 (wasm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.12s
     Running `node target/debug/wasm/web.mjs`
Listening on http://localhost:3000
```

### Kotlin declarations for npm packages

For each package in `[target.wasm.npm-dependencies]`, Kargo reads the package's own `.d.ts` files, or those of `@types/<name>` when it ships none (express, for example), and writes Kotlin/Wasm `external` declarations into `target/<profile>/generated/npm/`. They compile with your code, and `kargo lsp` reads them for completion, hover (with the package's documentation) and go to definition.

- **Packages.** Each npm package gets a Kotlin package named after it: `express` is `express`, `@acme/my-lib` is `acme.my_lib`. The package's default export is a function named after the package (`express()`, `leftPad()` for `left-pad`), and its named exports keep their names (`import express.Router`). The types a package uses from other packages, such as `express-serve-static-core`, get Kotlin packages of their own.
- **`@types`.** For a package without declarations of its own, Kargo installs `@types/<name>` at the package's major version (`@types/express@^5`) into `target/<profile>/npm-types/`, apart from the packages your program runs with. If there is no such package, the build warns and goes on without declarations for it. You can also list a `@types/` package in `npm-dependencies` yourself to pick its version.
- **When.** `kargo build`, `run`, `test` and `check` install the packages before compiling and generate the declarations again only when the installed packages change. `kargo lsp` does the same when it opens the project, so the editor has them before your first build.
- **Opt-in.** Kotlin/Wasm interop types (`JsAny` and friends) need the opt-in `kotlin.js.ExperimentalWasmJsInterop` since Kotlin 2.2. When a project has npm dependencies, Kargo passes it to the Wasm compiler, so neither the declarations nor your calls to them warn.

Kotlin/Wasm can only pass `String`, `Int`, `Double`, `Boolean`, function types and JS objects (`JsAny`) to JavaScript, so the TypeScript types are simplified:

| TypeScript | Kotlin |
| --- | --- |
| An interface or class | An `external interface` (`external class` for the package's own classes) with the members of all its supertypes; generic types take their default type arguments |
| A property whose type is callable, such as `get: IRouterMatcher<this>` | Methods, one per call signature, so `app.get("/") { ... }` works |
| An interface that is only callable, such as `RequestHandler` | A Kotlin function type, so you pass a lambda |
| `number` parameter | Overloads for `Int` and `Double` |
| `any` parameter | Overloads for `JsAny?` and `String` |
| A union parameter (`string \| RegExp`) | One overload per member; `JsAny` stands for the object types |
| A rest parameter `...handlers` | A `vararg`, and a single parameter so a trailing lambda works |
| An optional parameter | A default of `definedExternally`, or, with several types, an overload without it and one per type |
| Other unions, `object`, `Date`, conditional and mapped types | `JsAny` (or `JsAny?` for `any` and `unknown`) |
| `string[]`, `Promise<T>` | `JsArray<JsString>`, `Promise<...>` |
| A string index signature `[key: string]: T` | An `operator fun get(key: String)` extension in the package's Kotlin package, so `req.params["id"]` works with `import express.get` (or `import express.*`) |

A package without TypeScript declarations gets none, with a warning; declare what you use yourself with `@JsModule("<name>")` [external declarations](https://kotlinlang.org/docs/wasm-js-interop.html). The generated files are rewritten by the next build, so edit your own declarations instead of them.

### In the browser

Node.js finds packages in `node_modules` by itself; a browser needs to be told where they are. For a `browser` app Kargo adds an [import map](https://developer.mozilla.org/docs/Web/HTML/Element/script/type/importmap) to the page, which maps each installed package to its ES module (the `exports`, `module` or `main` entry of its `package.json`), and serves `node_modules` at `/node_modules/`. `kargo run` and `kargo test` both do this, and the page you put in `resources/index.html` gets the import map too, unless it has one of its own.

This works for packages that ship ES modules, such as `@js-joda/core`. A package that only ships CommonJS can't run in a browser without a bundler; bundle it yourself, or use the `js` environment and a bundler of your choice.

### Choosing npm, pnpm or bun

By default Kargo uses the first of `npm`, `pnpm` and `bun` it finds on your `PATH`. To pick one, set `package-manager`:

```toml title="kargo.toml"
[target.wasm]
package-manager = "bun"   # "npm", "pnpm" or "bun"
```

The package manager's lockfile (`package-lock.json`, `pnpm-lock.yaml` or `bun.lock`) stays in `target/<profile>/` and is kept when the packages are installed again, so a version range stays at the version it was first installed at until you delete it (or run `kargo clean`). When you switch package managers, Kargo deletes the old `node_modules` and lockfile and installs again. The packages always run on Node.js or in the browser, whichever tool installed them.

Kargo installs the packages with `--ignore-scripts`, so their `preinstall`, `install` and `postinstall` scripts never run: the packages come from the metadata of Maven libraries, which should not get to run programs on your machine. A package that needs its install script to work (to build a native addon, say) won't; Wasm programs rarely use such packages.

## Libraries and npm packages

A Wasm project can be a library for JavaScript and TypeScript code. Set `kind = "library"` and mark what you want to expose with `@JsExport`:

```toml title="kargo.toml"
[package]
name = "greeter"
version = "0.3.0"
license = "MIT"

[target.wasm]
environment = "browser"           # or "js"
kind = "library"
module-name = "greeter-core"      # the name of the files; default: the package name
npm-name = "@acme/greeter"        # the npm package name; default: the lowercase package name
```

```kotlin title="src/Greeter.kt"
package com.example.greeter

@OptIn(ExperimentalJsExport::class)
@JsExport
fun greet(name: String): String = "Hello, $name!"
```

```console
$ kargo build --release
   Compiling greeter v0.3.0 (wasm)
$ ls target/release/wasm
greeter-core.d.mts  greeter-core.import-object.mjs  greeter-core.js-builtins.mjs
greeter-core.mjs    greeter-core.wasm               package.json
```

`target/<profile>/wasm/` is now an npm package. Publish it with `npm publish target/release/wasm`, or pack it with `npm pack`, and use it like any other:

```js
import { greet } from "@acme/greeter";

console.log(greet("npm")); // Hello, npm!
```

- **`@JsExport`** marks the top-level functions that JavaScript can call. In Kotlin 2.4 Kotlin/Wasm exports functions only; the annotation does not apply to classes. Everything else is removed from release builds.
- **`greeter-core.d.mts`** holds TypeScript declarations for the exported functions (`export declare function greet(name: string): string;`).
- **`package.json`** is written for you: `name`, `version`, `description`, `license` and `author` come from `[package]`, `type` is `module`, and `main`, `module`, `types` and `exports` point at the files above. Its `dependencies` list the [npm packages](#npm-packages) the library needs, both those its Kotlin libraries ask for and your `npm-dependencies`.
- A library has no `main`, so `kargo run` refuses to run it. `kargo test` runs its tests like any other Wasm target, on Node.js or in a headless browser, according to `environment`.
- The module finds its `.wasm` next to itself, so keep the files together. `kind = "library"` needs `environment = "js"` or `"browser"`; a `wasi` module is not an npm package.

## Web resources from libraries

Some Kotlin/Wasm libraries are bindings to a module built separately, such as an Emscripten build of a C library (`raylib.mjs` and `raylib.wasm`). The app that uses the bindings needs those files next to its own module, and often a few lines of JavaScript that load them before the app starts. A library declares both in `[target.wasm]`:

```toml title="kargo.toml"
[package]
name = "raylib-kt"
version = "0.4.0"
group = "io.github.example"

[target.wasm]
environment = "browser"
kind = "library"
web-resources = ["web/raylib.mjs", "web/raylib.wasm"]   # files or directories shipped to every app using the library
web-init = "web/init.mjs"                              # runs before the app module is imported
```

```js title="web/init.mjs"
import createRaylib from './raylib.mjs';

const canvas = document.createElement('canvas');
canvas.id = 'canvas';
document.body.append(canvas);
globalThis.raylib = await createRaylib({ canvas });
```

An app adds the library like any other dependency, and nothing else:

```toml title="kargo.toml"
[target.wasm]
environment = "browser"

[dependencies]
raylib-kt = { path = "../raylib-kt" }   # or "io.github.example:raylib-kt" = "0.4.0"
```

`kargo build`, `kargo run` and `kargo test` put `raylib.mjs`, `raylib.wasm` and `init.mjs` in `target/<profile>/wasm/`, and the page Kargo generates imports every library's init script, one after the other, before the app module:

```html
<script type="module">
    // Generated by Kargo: the init scripts of the app's libraries, then the app.
    await import("./init.mjs");
    await import("./game.mjs");
</script>
```

- **Names.** Each file lands next to the app module under its own name (`web/raylib.mjs` becomes `raylib.mjs`), so the init script imports `./raylib.mjs`. A directory keeps its name and contents: `web/assets` becomes `assets/`. Two entries of one library with the same name are an error, as is an absolute path or one with `..`. The init script is shipped even when `web-resources` does not list it.
- **Order.** A library's init script runs after those of the libraries it depends on.
- **The app's own files win.** A file in the app's `resources/` replaces a library's file of the same name. Two libraries shipping the same name is an error that names both, and so is a library file named like one the build writes (the app's module files or `index.html`).
- **Your own page.** With a `resources/index.html`, the library files are still copied, but Kargo doesn't touch your page: import the init scripts yourself, before the app module.
- **Tests.** The page that runs `kargo test` in a browser imports the init scripts too, including the package's own when you test the library itself.
- **Release builds** copy the files as they are. They are usually optimized already (Emscripten's `-Os`), so Kargo never runs `wasm-opt` over them.
- **Removed files** don't linger: a file a library no longer ships is deleted from the output on the next build.
- `web-resources` and `web-init` need `kind = "library"` and the `js` or `browser` environment. Only browser apps receive the files.

### Path dependencies and publishing

A [path dependency](../dependencies/#local-packages)'s files are copied from its package. They have to exist: a declared file that is missing is an error that names it. A path dependency is built without its `pre-build` hooks, so if one of its tasks produces the files (an Emscripten build, say), run it in the library first.

[`kargo publish`](../publishing/) puts the files in a zip next to the library's klib, under the `web-resources` classifier (`raylib-kt-wasm-js-0.4.0-web-resources.zip`), so they always match the bindings of the same version. The POM of the Wasm module says so with the properties `kargo.web-resources` and `kargo.web-init` (the name of the init script). When an app resolves the library from a repository, Kargo reads that POM, downloads the zip, records its SHA-256 in [`kargo.lock`](../../reference/lockfile/) and unpacks it into the output. `kargo fetch` downloads the zips too, so an offline build has them. The zip is not part of any variant of the Gradle module metadata, so Gradle builds that use the library ignore it.

## Release builds

`kargo build --release -t wasm` (and `kargo run --release`) produces a much smaller module than the default dev build:

1. **Dead code is removed.** The release profile links with Kotlin's dead code elimination: only what `main` or `@JsExport` can reach stays.
2. **Pinned Binaryen optimizes the module.** Kargo downloads and SHA-256 checks its pinned release into the toolchain cache when it is not already installed. `opt-level` chooses `-Oz`, `-O2` or `-O3`; Kargo always enables the Wasm GC and exception-handling features the Kotlin output needs.
3. **Kargo reports the sizes** of the module, its gzip and Brotli representations and the whole output directory. `max-size` and `max-gzip-size` make a release fail when it exceeds a budget. `kargo build --release --size-report` asks Binaryen to print module metrics and function-level metrics to help identify large functions.

```console
$ kargo build --release
   Compiling hello v0.1.0 (wasm)
   Downloading binaryen-version_133-x86_64-linux.tar.gz
   Optimizing hello.wasm with wasm-opt
   Optimized hello.wasm: 27.2 KiB -> 1.3 KiB
        Size hello.wasm 1.3 KiB (711 B gzip, 690 B brotli), 5 files 8.8 KiB in target/release/wasm
    Finished `release` profile [optimized] target(s) in 0.86s
```

`[target.wasm] wasm-opt = false` disables Binaryen; a string selects a custom executable. `debug-info = true` preserves debug names and writes `<name>.wasm.map` for release crash reports. By default release output strips debug data and source maps. `compress = ["gzip", "brotli"]` creates compressed copies of the Wasm module and `.mjs` glue. The browser server negotiates those copies with `Content-Encoding`.

```toml title="kargo.toml"
[target.wasm]
wasm-opt = false                 # never run it
# wasm-opt = "tools/wasm-opt"    # run this executable (relative to the project), not the one on PATH
opt-level = "size"               # or "balanced" / "speed"
compress = ["gzip", "brotli"]
content-hash = true              # app.<hash>.wasm and app.<hash>.mjs for long-lived CDN caching
max-size = "1 MiB"
max-gzip-size = "256 KiB"
```

`wasm-opt-flags` adds Binaryen passes. `wasm-enable-array-range-checks` can explicitly override the release default, and `wasm-debugger-custom-formatters` enables Kotlin's browser debugger formatters. The dev profile stays fast and keeps the compiler's incremental klib cache; Binaryen optimization, compression and budgets apply to optimized builds.

## Module name and output files

A build writes these files to `target/<profile>/wasm/`, named after the package or after `module-name`:

| File | What it is |
|---|---|
| `<name>.mjs` | The ES module that loads the Wasm module and runs `main` |
| `<name>.wasm` | The compiled program |
| `<name>.import-object.mjs`, `<name>.js-builtins.mjs` | JavaScript the module imports; keep them next to it |
| `<name>.d.mts` | TypeScript declarations (libraries only) |
| `package.json` | The npm package (libraries only) |
| `index.html` and your `resources/` | The page of a `browser` app |
| The libraries' web resources | The files your [libraries ship](#web-resources-from-libraries) to a `browser` app |

Set `module-name` to change `<name>`, for example when the package name is not what the files should be called. It may contain letters, digits, `-`, `_` and `.`.

## Calling JavaScript (environments `js` and `browser`)

```kotlin title="src-wasm/Console.kt"
@file:OptIn(ExperimentalWasmJsInterop::class)

package com.example.hello

// Bind to an existing JavaScript function.
@JsFun("(message) => console.warn(message)")
external fun warn(message: String)

fun shout(text: String) = warn(text.uppercase())
```

The Kotlin docs cover [JavaScript interop for Wasm](https://kotlinlang.org/docs/wasm-js-interop.html) in depth.

## Limitations

- **Compose for Web** needs `environment = "browser"`. See [Compose for the web](../compose/#compose-for-the-web).
- Libraries must publish a Wasm variant (`wasmJs` or `wasmWasi`). JVM-only libraries can't be used.
- Test name filters are ignored on Wasm; all tests run.
- Packages that only ship CommonJS can't be used in the `browser` environment without bundling them yourself.
- `kargo run` serves a browser app as it is. It doesn't rebuild or reload the page when your sources change; run it again.
