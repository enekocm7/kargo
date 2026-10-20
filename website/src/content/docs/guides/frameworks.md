---
title: Built-in frameworks
description: Enable Compose, Ktor, Spring Boot, kotlinx.serialization, coroutines, Lombok and benchmarks with one line each.
---

Some libraries need more than a dependency: compiler plugins, extra flags, or a set of matching artifacts. Kargo knows how to set these up. Turn them on in `[frameworks]`:

```toml title="kargo.toml"
[frameworks]
serialization = true
coroutines = true
```

From the command line:

```sh
kargo add --framework serialization,coroutines
kargo new myapp -w ktor,serialization       # when creating a project
```

Each framework adds what it needs for every target that supports it. Use `kargo tree` to see exactly what was added.

| Framework | JVM | Wasm | Native | Android |
|---|:-:|:-:|:-:|:-:|
| `serialization` | ✓ | ✓ | ✓ | ✓ |
| `coroutines` | ✓ | ✓ | ✓ | ✓ |
| `ktor` | ✓ | client | ✓ (CIO) | client |
| `compose` | desktop | | | Jetpack Compose |
| `spring-boot` | ✓ | | | |
| `lombok` | ✓ | | | ✓ |
| `benchmark` | ✓ | | | |

## kotlinx.serialization

```toml
[frameworks]
serialization = true
# or
serialization = { version = "1.11.0", formats = ["json", "cbor", "protobuf"] }
```

Adds the serialization compiler plugin and one library per format. Formats: `json` (the default), `protobuf`, `cbor`, `properties`, `hocon` (JVM only) and `core`.

```kotlin
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

@Serializable
data class User(val name: String, val age: Int)

fun main() {
    val json = Json.encodeToString(User("Ada", 36))      // {"name":"Ada","age":36}
    val user = Json.decodeFromString<User>(json)
}
```

## Coroutines

```toml
[frameworks]
coroutines = true      # or a version: coroutines = "1.11.0"
```

Adds `kotlinx-coroutines-core`, plus `kotlinx-coroutines-test` for JVM tests and `kotlinx-coroutines-swing` when used with Compose Desktop.

```kotlin
import kotlinx.coroutines.*

fun main() = runBlocking {
    val results = (1..3).map { n -> async { delay(100L * n); n * n } }.awaitAll()
    println(results)   // [1, 4, 9]
}
```

## Ktor

```toml
[frameworks]
ktor = { server = "netty" }
# all options:
ktor = { version = "3.6.0", server = "netty", client = "cio", server-plugins = ["cors", "call-logging"], client-plugins = ["logging"], json = true }
```

| Key | Effect |
|---|---|
| `server` | Adds `ktor-server-core` and `ktor-server-<engine>` (`netty`, `cio`, `jetty`, ...). Native supports only `cio`. On the JVM it also adds Logback and `ktor-server-test-host` for tests. |
| `client` | Adds `ktor-client-core` and `ktor-client-<engine>` (`cio`, `okhttp`, `java`, `js`, ...). |
| `server-plugins` / `client-plugins` | Each name adds `ktor-server-<name>` / `ktor-client-<name>`. |
| `json` | Adds `ktor-serialization-kotlinx-json` and content negotiation. Defaults to `true` when `serialization` is also enabled. |

See [A REST API with Ktor](../ktor/) for a full walkthrough.

## Spring Boot

```toml
[frameworks]
spring-boot = true
# or
spring-boot = { version = "4.1.1", starters = ["web", "data-jpa", "actuator"] }
```

Imports the Spring Boot BOM, adds `spring-boot-starter-<name>` for each starter (default `["web"]`), `kotlin-reflect`, Jackson's Kotlin module for web starters (`com.fasterxml.jackson.module` for Spring Boot 3, `tools.jackson.module` for Spring Boot 4 and newer, which use Jackson 3) and `spring-boot-starter-test` for tests. It also configures the `allopen` compiler plugin with the Spring preset, the `noarg` plugin for JPA entities (when `data-jpa` is used, or `jpa = true`), and the `-Xjsr305=strict` and `-java-parameters` flags. JVM only.

See the [Spring Boot guide](../spring-boot/).

## Compose

```toml
[frameworks]
compose = true
# or
compose = { version = "1.12.1", material3 = true, resources-package = "com.example.res", public-resources = false }
```

- On the **JVM** it sets up [Compose Multiplatform](https://www.jetbrains.com/compose-multiplatform/) for desktop: the Compose compiler plugin, the desktop runtime for your OS and Material 3 (disable with `material3 = false`).
- On **Android** it enables Jetpack Compose in the generated Gradle project.
- On both, a `composeResources/` directory gets the generated `Res` class of Compose Multiplatform resources (`Res.drawable.*`, `Res.string.*`, ...) and the resources runtime. `resources-package` and `public-resources` set the package and the visibility of `Res`.
- `kargo package --desktop` builds an installer (or with `--platforms all` a jar for every OS) of a desktop application.
- Compose is not available on the Wasm or Native targets.

See the [Compose Desktop guide](../compose/).

## Lombok

```toml
[frameworks]
lombok = true
# or
lombok = { version = "1.18.48", config = "lombok.config" }
```

Adds Lombok as a `provided` dependency and annotation processor for main and test Java sources, and the Kotlin Lombok compiler plugin so Kotlin code can see the generated getters, setters and builders. A `lombok.config` in the project root is picked up automatically. JVM and Android only.

## Benchmarks

```toml
[frameworks]
benchmark = true
# or
benchmark = { version = "0.5.0" }
```

Adds kotlinx-benchmark's annotations (`@Benchmark`, `@State`, `Blackhole`) and JMH as dev-dependencies of the JVM target, so your editor resolves them in `benches/`. `kargo bench` runs without this framework too, with the same libraries; the framework is for the editor and to pin the version. `kargo new --bench` (or `-w benchmark`) and `kargo add --framework benchmark` also write a sample benchmark in `benches/SampleBench.kt` when there is none. JVM only; see [Benchmarking](../benchmarking/).

## Combining frameworks

Frameworks combine. A few rules apply:

- Pick `spring-boot` **or** `ktor` for a server, not both.
- `spring-boot` projects are applications, so they can't be created with `--lib`.
- `spring-boot`, `lombok` and `compose` can't be used in projects that enable the Wasm or Native targets for that code.

Something not covered here? Any compiler plugin can be added by hand; see [Tasks and plugins](../tasks-and-plugins/#compiler-plugins).
