---
title: Examples
description: Complete sample projects for the JVM, Ktor, Spring Boot, Compose, Android, Wasm, JS and Native that you can copy and run.
---

The [`examples/`](https://github.com/enekocm7/kargo/tree/master/examples) folder of the repository holds complete projects, each with its own `kargo.toml`, sources, tests and a README with the commands to run it. Copy one out and run it:

```sh
git clone https://github.com/enekocm7/kargo
cp -r kargo/examples/ktor-server ~/ktor-server
cd ~/ktor-server
kargo run
```

| Example | Shows | Targets | Guide |
| --- | --- | --- | --- |
| `hello-jvm` | The smallest project: an application and a test | JVM | [Your first Kotlin app](../first-app/) |
| `ktor-server` | A Ktor server with kotlinx.serialization | JVM | [A REST API with Ktor](../ktor/) |
| `spring-boot` | A Spring Boot web application | JVM | [Spring Boot](../spring-boot/) |
| `compose-desktop` | A Compose Multiplatform desktop window | JVM | [Compose](../compose/) |
| `compose-multiplatform` | One shared `App()` on desktop and in the browser | JVM, Wasm | [Compose](../compose/) |
| `compose-html` | Compose HTML rendering DOM elements | JS | [Compose](../compose/) |
| `android` | An Android app, with no Gradle files | Android | [Android](../android/) |
| `kotlin-wasm` | A browser app on Kotlin/Wasm | Wasm | [WebAssembly](../wasm/) |
| `kotlin-js` | A Kotlin/JS application | JS | [JavaScript (Kotlin/JS)](../js/) |
| `kotlin-native` | A native executable | Native | [Kotlin/Native](../native/) |
| `multiplatform-library` | A serializable library for three targets | JVM, JS, Wasm | [Multiplatform projects](../multiplatform/) |
| `workspace` | Two packages joined by a path dependency | JVM | [Workspaces](../workspaces/) |

## They match Kargo's defaults

None of the examples pins a Kotlin, framework or JVM version, so they build with the versions of the Kargo you have installed. Apart from `workspace`, each one is exactly what [`kargo new`](../../reference/commands/) writes for the same flags (`kargo new ktor-server --with ktor,serialization`, for instance), so you can also generate a fresh copy yourself.

## For contributors

`cargo test --test examples` checks offline that every example still equals the output of `kargo new`, that no `kargo.toml` pins a version other than Kargo's default, and that each one loads. When a template changes, refresh the examples:

```sh
KARGO_UPDATE_EXAMPLES=1 cargo test --test examples
```

The `Examples` workflow builds and tests every example with the real compilers on each change to `examples/`, on pull requests that touch the templates, and once a week. See [Contributing](../../reference/contributing/).
