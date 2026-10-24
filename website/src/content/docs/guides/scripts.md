---
title: Kotlin scripts
description: Run a single Kotlin file with its dependencies, without a project, using kargo script.
---

`kargo script` runs one Kotlin file without a project: a quick tool, a data fix or a CI step. The file declares what it needs, and Kargo does the rest. It resolves the dependencies with the same resolver as a project, uses the same JDKs and caches, and keeps the compiled result, so a second run of an unchanged script starts without compiling.

```kotlin title="fetch.kts"
#!/usr/bin/env -S kargo script
@file:DependsOn("org.jetbrains.kotlinx:kotlinx-coroutines-core-jvm:1.10.2")

import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking

runBlocking { delay(1) }
println("hello ${args.joinToString("|")}")
```

```console
$ kargo script fetch.kts world --flag
hello world|--flag
$ ./fetch.kts world
hello world
```

The shebang line lets you run the file directly on Linux and macOS, where `env -S` passes `kargo script` its arguments. On Windows, run it as `kargo script fetch.kts`.

Anything after the file name goes to the script as `args`. An argument that starts with `-` needs `--` before it when you use `-e`, as in `kargo script -e 'println(args.size)' -- --flag`.

To run code straight from the command line, use `-e`:

```console
$ kargo script -e 'println(1 + 1)'
2
```

## Declare what the script needs

A `.kts` script declares its settings with `@file:` annotations at the top of the file, before any code. Kargo reads them and removes them before the Kotlin compiler sees the file, so they work the same as in `kotlin-main-kts` scripts.

| Annotation | Meaning |
|---|---|
| `@file:DependsOn("group:artifact:version")` | A Maven dependency. The version is required, so the script always runs the same code |
| `@file:Repository("https://...")` | An extra Maven repository. `"mavenCentral"` names the default one |
| `@file:Kotlin("2.4.21")` | The Kotlin version to compile with. Default: the version Kargo uses for projects |
| `@file:Jdk("21")` | The JDK to compile and run with: a major version, a release such as `"21.0.5+11"`, or `"system"` for `JAVA_HOME` or `java` on `PATH`. Default: `system` |
| `@file:Framework("coroutines")` | A [built-in framework](../frameworks/). Those that need a Kotlin compiler plugin, `serialization`, `compose`, `lombok` and `spring-boot`, need a `.kt` file instead; see [Limits](#limits) |

Other `@file:` annotations, such as `@file:Suppress`, stay in the file for the Kotlin compiler.

`.main.kts` files run the same way: a `kotlin-main-kts` script that uses `@file:DependsOn` and `@file:Repository` runs unchanged with `kargo script`.

## Kotlin files with a front matter

A `.kt` file keeps its `fun main` and declares its settings in a `/* kargo ... */` comment at the top, in TOML:

```kotlin title="tool.kt"
/* kargo
[dependencies]
"org.jetbrains.kotlinx:kotlinx-coroutines-core-jvm" = "1.10.2"
*/

import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking

fun main(args: Array<String>) {
    println("tool " + runBlocking { async { args.joinToString() }.await() })
}
```

```console
$ kargo script tool.kt a b
tool a, b
```

The front matter takes the same settings as the annotations, with these keys: `kotlin`, `jdk`, `repositories` (a list of URLs), `frameworks` (a list of names) and `dependencies` (a table from `"group:artifact"` to the version). A `.kt` file without a front matter needs no settings at all. Unlike a `.kts` script, a `.kt` file can use any framework, including those that need a compiler plugin.

Use a `.kt` file when the script grows into several declarations or needs a compiler plugin. Its `main` runs the same way it would in a project.

## How runs are cached

Kargo keeps each script's compiled classes in `~/.kargo/cache/scripts`, one directory per script. The directory is keyed by the script's code and everything that changes what it compiles to: its Kotlin version, JDK request, repositories, dependencies and frameworks. Changing any of them compiles the script again. `KARGO_HOME` moves the cache with the rest of Kargo's files.

The first run of a `.kts` script compiles it, and the Kotlin compiler runs the script as part of that step, so the first run takes a few seconds longer. Later runs start the compiled classes with `java` and do not compile, so they start in JVM-startup time. Output and exit codes are the same either way: a script that throws exits with `1`, and one that does not compile exits with `101`.

A `.kt` file is compiled once and then started the same way.

A script exits when it returns, even if it started threads that are still running, as the `kotlin` command does. A library that leaves a non-daemon thread behind, such as an HTTP client's dispatcher, therefore doesn't keep the terminal waiting.

With `--offline`, a script that has run before works from the cache without the network. A script that has never run needs the network to resolve its dependencies and compiler.

:::note
Compiler messages give the line numbers of your file. The path they show is Kargo's copy of the script under the cache directory, because Kargo removes the annotations from that copy before compiling it.
:::

## Run a file with `kargo run`

`kargo run <file>` runs a script the same way: `kargo run fetch.main.kts world` is `kargo script fetch.main.kts world`, and `kargo run -e 'println(1 + 1)'` is `kargo script -e`. A `.kt` file runs the same way too when it has a front matter or lies outside any project.

A `.kt` file without a front matter inside a project is different: `kargo run tools/migrate.kt` compiles it against the project's code and dependencies instead, without adding it to the build. See [Run a single file](../multiple-programs/#run-a-single-file).

## Editors

The [language server](../editors/#language-server) treats a `.kts` file as a Kargo script when it lies outside any project, or when its shebang runs `kargo script`. A `.kts` file inside a project is a build script that Kargo does not run, so the server leaves it alone unless its shebang says otherwise. In Zed, a folder with only scripts needs a setting to start the server; see [Kargo scripts in Zed](../editors/#kargo-scripts-in-zed).

For a Kargo script, the server completes the annotation names, the values of `@file:Jdk`, `@file:Framework` and `@file:Repository`, the coordinates and versions of `@file:DependsOn`, and the versions of `@file:Kotlin`. It shows what each annotation means on hover, and it reports a header that `kargo script` would reject, such as a dependency without a version, as a diagnostic on that annotation.

The rest of the file goes to the Kotlin server, which reads its top-level statements and `args` as it runs the script. The libraries of its `@file:DependsOn` declarations are loaded for that script alone, so completion, hover and go to definition work on them, while the project's own files don't see them. The [Kotlin server](../kotlin-server/) page explains where those libraries come from.

## Limits

- `@file:Import` is not supported yet. Put the shared code in a `.kt` file in a project, or copy it into the script.
- Compiler plugins do not run in a `.kts` script. `@file:Framework("serialization")`, `"compose"`, `"lombok"` and `"spring-boot"` need a `.kt` file, which runs them the way a project does. `"coroutines"` and `"ktor"` are libraries and work in scripts.
- A `.kts` script's dependencies come from its annotations only. A script has no `kargo.toml`, so it does not use a project's `[dependencies]`, `[toolchain]` or `[env]` settings.
- Each `@file:DependsOn` needs an exact version. Kargo does not resolve the latest one.
- In the editor, the libraries of `@file:Framework` are not loaded, so the classes they add don't complete there yet. `kargo script` runs them.
- Kargo cannot turn a script into a project yet. When a script outgrows one file, create a project with `kargo new` and move the code into it.
