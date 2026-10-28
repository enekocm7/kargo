---
title: Frameworks
description: Every key of [frameworks] and the exact artifacts, compiler plugins and flags each built-in framework adds to each target.
---

`[frameworks]` turns on the [built-in frameworks](../../guides/frameworks/). This page is the lookup: the keys of each framework, and what it adds to each target. For a walkthrough, read [Built-in frameworks](../../guides/frameworks/) first. For every other `kargo.toml` section, see the [manifest reference](../manifest/).

Each framework is `true`, `false` or a table of options. The `version` of a framework is one exact version. Use `kargo tree -t <target>` to list what a framework added to a target, and `kargo why <group:artifact>` to see which framework asked for an artifact.

| Framework | Default version | JVM | Wasm | JS | Native | Android |
|---|---|:-:|:-:|:-:|:-:|:-:|
| [`serialization`](#serialization) | 1.11.0 | ✓ | ✓ | ✓ | ✓ | ✓ |
| [`coroutines`](#coroutines) | 1.11.0 | ✓ | ✓ | ✓ | ✓ | ✓ |
| [`ktor`](#ktor) | 3.6.0 | server, client | client | client | server (CIO), client | client |
| [`compose`](#compose) | 1.12.1 (Material 3 1.9.0) | desktop | browser only | | iOS framework | Jetpack Compose |
| [`spring-boot`](#spring-boot) | 4.1.1 | ✓ | | | | |
| [`lombok`](#lombok) | 1.18.48 | ✓ | | | | ✓ |
| [`benchmark`](#benchmark) | 0.5.0 (JMH 1.37) | ✓ | | | | |

A framework a target does not support is an error when you build that target (`the Ktor server is only available on the JVM and Kotlin/Native targets`), not a silent skip. Android is built through Gradle, which gets the same libraries and the Gradle plugins that match the framework.

## `serialization`

```toml
[frameworks]
serialization = true
serialization = { version = "1.11.0", formats = ["json", "cbor"] }
```

| Key | Default | Meaning |
|---|---|---|
| `version` | `1.11.0` | The kotlinx.serialization version |
| `formats` | `["json"]` | `json`, `protobuf`, `cbor`, `properties`, `hocon` (JVM only; other targets skip it) and `core` (no format) |

Adds the serialization compiler plugin on every target, and `org.jetbrains.kotlinx:kotlinx-serialization-<format>` for each format.

## `coroutines`

```toml
[frameworks]
coroutines = true
coroutines = "1.11.0"
```

Adds `kotlinx-coroutines-core` on every target. On the JVM it also adds `kotlinx-coroutines-test` for tests, and `kotlinx-coroutines-swing` when `compose` is enabled too.

## `ktor`

```toml
[frameworks]
ktor = { server = "netty" }
ktor = { client = "cio", client-engines = { native = "darwin", wasm = "none" } }
ktor = { version = "3.6.0", server = "netty", client = "okhttp", server-plugins = ["cors"], client-plugins = ["logging"], json = true }
```

| Key | Default | Meaning |
|---|---|---|
| `version` | `3.6.0` | The Ktor version, for every Ktor artifact |
| `server` | none | The server engine: `netty`, `cio`, `jetty` or `tomcat`. Kotlin/Native supports only `cio` |
| `client` | none | The client engine: `cio`, `okhttp`, `java`, `apache5`, `js`, `curl`, `winhttp` or `darwin`. It applies to every target without an entry in `client-engines` |
| `client-engines` | none | The client engine of single targets (`jvm`, `android`, `wasm`, `js`, `native`), instead of `client`. An engine name, or `none` for `ktor-client-core` alone. Needs `client` |
| `server-plugins` | `[]` | Each name adds `io.ktor:ktor-server-<name>` |
| `client-plugins` | `[]` | Each name adds `io.ktor:ktor-client-<name>` |
| `json` | `true` when `serialization` is enabled | Adds `ktor-serialization-kotlinx-json`, and the content-negotiation plugin of the server and the client unless you list it in the plugins |

Set at least one of `server` and `client`. Both take the **name** of an engine, a string. `client = true` is an error that says so, since `true` would not say which engine to use.

### What each target gets

| Target | `server` | `client` |
|---|---|---|
| JVM | `ktor-server-core`, `ktor-server-<engine>`, the plugins, Logback (runtime) and `ktor-server-test-host` (tests) | `ktor-client-core`, `ktor-client-<engine>`, the plugins |
| Android | An error: the server is not available | `ktor-client-core`, `ktor-client-<engine>`, the plugins |
| Wasm | An error | `ktor-client-core`, `ktor-client-<engine>`, the plugins |
| JS | An error | `ktor-client-core`, `ktor-client-<engine>`, the plugins |
| Native | `ktor-server-core`, `ktor-server-cio`, the plugins. Other engines are an error | `ktor-client-core`, `ktor-client-<engine>`, the plugins |

`<engine>` is the `client` engine of the whole project, or the `client-engines` entry of the target. Kargo adds the artifact you name and does not check that the engine runs on the target: that is up to you, and to Ktor. The engines for each target, from Ktor's own documentation:

| Target | Engines that run on it |
|---|---|
| JVM | `cio`, `okhttp`, `java`, `apache5` |
| Android | `okhttp`, `cio`, `android` |
| Wasm and JS | `js`, which is part of Ktor's core for Wasm, so a Wasm target needs `none` and no extra artifact |
| Native (Apple) | `darwin`, `cio`, `curl` |
| Native (Linux, Windows) | `cio`, `curl`, `winhttp` (Windows) |

### Different engines for different targets

`client = "cio"` alone adds `ktor-client-cio` to every target that builds, the browser and iOS included. For a project with several targets, give each one the engine it runs on:

```toml title="kargo.toml"
[frameworks]
serialization = true
ktor = { client = "cio", client-engines = { android = "okhttp", native = "darwin", wasm = "none" } }
```

| Target | Gets |
|---|---|
| JVM | `ktor-client-core`, `ktor-client-cio` (the `client` default) |
| Android | `ktor-client-core`, `ktor-client-okhttp` |
| Native | `ktor-client-core`, `ktor-client-darwin` (iOS) |
| Wasm | `ktor-client-core` alone, which carries the browser's engine |

The JSON support and the plugins are not per target: `ktor-serialization-kotlinx-json`, `ktor-client-content-negotiation` and the `client-plugins` go to every target.

To go further, such as a Linux and a Windows engine in one project, leave `client-engines` to the target you can describe and add the rest by hand in `[target.<name>.dependencies]` ([per-target dependencies](../manifest/#per-target-dependencies)), with the same version as `[frameworks.ktor] version`:

```toml title="kargo.toml"
[target.native.dependencies]
"io.ktor:ktor-client-winhttp" = "3.6.0"
```

A project that builds a Ktor server and an app that calls it is usually two packages in a [workspace](../../guides/workspaces/), each with its own `[frameworks.ktor]`.

See [A REST API with Ktor](../../guides/ktor/) for the server.

## `compose`

```toml
[frameworks]
compose = true
compose = { version = "1.12.1", material3 = true, resources-package = "com.example.res", public-resources = false }
compose = { stability-config = "compose-stability.conf", strong-skipping = true, feature-flags = ["OptimizeNonSkippingGroups"], metrics = true, reports = true }
```

The keys are in the [manifest reference](../manifest/#frameworks) and the [Compose guide](../../guides/compose/#compiler-options).

| Target | Gets |
|---|---|
| JVM | The Compose compiler plugin, `org.jetbrains.compose.desktop:desktop-jvm-<os>-<arch>` for the machine, Material 3 (unless `material3 = false`), and with `coroutines` also `kotlinx-coroutines-swing` |
| Wasm | The Compose compiler plugin, `runtime`, `foundation`, `ui` and Material 3. Needs `environment = "browser"` in `[target.wasm]` |
| JS | An error: Compose for Web runs on Wasm |
| Native | The same libraries as Wasm, for the iOS platforms (`ios_arm64`, `ios_simulator_arm64`, `ios_x64`) with `kind = "framework"` |
| Android | Jetpack Compose in the generated Gradle project |

A `composeResources/` directory adds the resources runtime and the generated `Res` class on every target that has Compose. Compose needs Google Maven, which Kargo adds to `[repositories]`.

## `spring-boot`

```toml
[frameworks]
spring-boot = true
spring-boot = { version = "4.1.1", starters = ["web", "data-jpa"], jpa = true }
```

| Key | Default | Meaning |
|---|---|---|
| `version` | `4.1.1` | The Spring Boot version, which picks the BOM |
| `starters` | `["web"]` | Each name adds `spring-boot-starter-<name>`; the empty string is `spring-boot-starter`. A starter renamed in the chosen version (`aop` is `aspectj` in Spring Boot 4) is an error that names its replacement |
| `jpa` | `true` when `data-jpa` is a starter | The JPA `noarg` and `allopen` presets |

JVM only. Adds the Spring Boot BOM, the starters, `kotlin-reflect`, Jackson's Kotlin module for the web starters, `spring-boot-starter-test` for tests, the `allopen` (Spring preset) and `noarg` compiler plugins, and the flags `-Xjsr305=strict` and `-java-parameters` (`-parameters` for `javac`).

## `lombok`

```toml
[frameworks]
lombok = true
lombok = { version = "1.18.48", config = "lombok.config" }
```

JVM and Android only. Adds Lombok as a `provided` dependency and annotation processor, and the Kotlin Lombok compiler plugin. A `lombok.config` in the project root is picked up; `config` points to another file, which must exist.

## `benchmark`

```toml
[frameworks]
benchmark = true
benchmark = { version = "0.5.0" }
```

JVM only. Adds kotlinx-benchmark and JMH as dev-dependencies, for `benches/`. See [Benchmarking](../../guides/benchmarking/).
