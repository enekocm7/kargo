---
title: Quick start
description: Create, run, test and package a Kotlin project in five minutes.
---

This page walks through the everyday commands. It assumes you have [installed Kargo](../installation/).

:::tip
Starting from a Maven, Gradle or Kotlin Toolchain project? Run `kargo import` in its directory instead of creating a new project: it writes the `kargo.toml` and moves the sources. See [Importing a project](../../guides/importing/).
:::

## Create a project

```console
$ kargo new hello
     Created application `hello` package
$ cd hello
```

Kargo creates this:

```text
hello/
├── kargo.toml
├── .gitignore
├── src/
│   └── Main.kt
└── tests/
    └── GreetingTest.kt
```

`src/Main.kt` contains a small program:

```kotlin title="src/Main.kt"
package com.example.hello

fun greeting(name: String): String = "Hello, $name!"

fun main() {
    println(greeting("Kotlin"))
}
```

## Run it

```console
$ kargo run
   Compiling hello v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.47s
     Running `java com.example.hello.MainKt`
Hello, Kotlin!
```

The first run downloads the Kotlin compiler, so it takes a little longer. Later runs only recompile when something changed.

Pass arguments to your program after `--`:

```sh
kargo run -- --name World
```

## Test it

`tests/GreetingTest.kt` uses [`kotlin.test`](https://kotlinlang.org/api/core/kotlin-test/), which works on every target:

```kotlin title="tests/GreetingTest.kt"
package com.example.hello

import kotlin.test.Test
import kotlin.test.assertEquals

class GreetingTest {
    @Test
    fun greetsByName() {
        assertEquals("Hello, Kargo!", greeting("Kargo"))
    }
}
```

```sh
kargo test
```

## Add a dependency

```console
$ kargo add org.jetbrains.kotlinx:kotlinx-datetime
```

Without a version, Kargo picks the latest release and writes it to `kargo.toml`:

```toml title="kargo.toml"
[dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
```

The resolved versions and checksums go into `kargo.lock`. Commit both files.

## Build and package

```sh
kargo build              # debug build in target/debug/jvm/
kargo build --release    # release build in target/release/jvm/
kargo package --release  # one runnable fat jar
java -jar target/release/jvm/hello-0.1.0-all.jar
```

## Other templates

```sh
kargo new mylib --lib                    # a library instead of an app
kargo new app -t jvm,wasm,native         # several targets at once
kargo new api -w ktor                    # a Ktor server
kargo new web -w spring-boot             # a Spring Boot app
kargo new ui -w compose                  # a Compose Desktop app
kargo new phone -t android -w compose    # a Jetpack Compose Android app
kargo init                               # set up the current directory instead
```

## Cheat sheet

| Command | Short | What it does |
|---|---|---|
| `kargo build` | `kargo b` | Compile |
| `kargo run` | `kargo r` | Build and run |
| `kargo test` | `kargo t` | Build and run tests |
| `kargo add <coords>` | | Add a dependency |
| `kargo remove <coords>` | `kargo rm` | Remove a dependency (`-F <framework>`: disable a framework) |
| `kargo outdated` | | List dependencies with newer versions |
| `kargo upgrade` | | Raise versions in `kargo.toml` |
| `kargo tree` | | Show the dependency graph |
| `kargo clean` | | Delete `target/` |
| `kargo info` | | Show project information |

Full details are in the [command reference](../../reference/commands/).
