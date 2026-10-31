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
| `@file:Target("native")` | Build for this target: `"jvm"` (the default) or `"native"` for a native executable of a `.kt` file; see [Native scripts](#native-scripts). A `.kts` script with `"native"` is an error |

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

The front matter takes the same settings as the annotations, with these keys: `kotlin`, `jdk`, `repositories` (a list of URLs), `frameworks` (a list of names), `target` (`"jvm"` or `"native"`; see [Native scripts](#native-scripts)) and `dependencies` (a table from `"group:artifact"` to the version). A `.kt` file without a front matter needs no settings at all. Unlike a `.kts` script, a `.kt` file can use any framework, including those that need a compiler plugin.

Use a `.kt` file when the script grows into several declarations or needs a compiler plugin. Its `main` runs the same way it would in a project.

## Native scripts

A `.kt` file with a `fun main` also runs as a native executable for this machine, built with the Kotlin/Native toolchain Kargo already downloads:

```kotlin title="tool.kt"
/* kargo
target = "native"
[dependencies]
"org.jetbrains.kotlinx:kotlinx-coroutines-core" = "1.11.0"
*/

fun main(args: Array<String>) {
    println("native tool " + args.joinToString())
}
```

```console
$ kargo script --target native tool.kt a b
native tool a, b
```

`--target native` picks the native executable; without it the file runs on the JVM. `target = "native"` in the front matter, or `@file:Target("native")` in a `.kts` header, picks it without the flag, and `--target jvm` runs a native-front-matter file on the JVM instead. `kargo run tool.kt -t native` runs a script file natively the same way.

The dependencies resolve for the host platform (`linux_x64`, `macos_arm64`, `mingw_x64`), so only klib-based libraries work: multiplatform libraries such as `kotlinx-coroutines-core` resolve to their native variant, while a JVM-only artifact such as `kotlinx-coroutines-core-jvm` does not resolve. Frameworks that only run on the JVM, such as `spring-boot` and `lombok`, are errors for a native script, as they are for the Native target of a project.

The first native run compiles the file with `konanc` and links the binary, which takes longer than a JVM compile (and builds the standard library cache once per Kotlin version). The second run of an unchanged script starts the cached binary without rebuilding. Exit codes and stdout/stderr match the JVM path: the program's exit code is passed through, and a file that does not compile exits with `101`.

`.kts` scripts and `kargo script -e` stay on the JVM: the Kotlin scripting compiler is JVM-only, and a `.kts` file's top-level statements have no `fun main` to link. A `.kts` file with `@file:Target("native")` is an error that says to move the code to a `.kt` file.

## How runs are cached

Kargo keeps each script's output in `~/.kargo/cache/scripts`, one directory per script. The directory is keyed by the script's code and everything that changes what it compiles to: its Kotlin version, JDK request, repositories, dependencies, frameworks and target (with the host platform for `native`). Changing any of them compiles the script again. `KARGO_HOME` moves the cache with the rest of Kargo's files.

The first run of a `.kts` script compiles it, and the Kotlin compiler runs the script as part of that step, so the first run takes a few seconds longer. Later runs start the compiled classes with `java` and do not compile, so they start in JVM-startup time. Output and exit codes are the same either way: a script that throws exits with `1`, and one that does not compile exits with `101`.

A `.kt` file is compiled once and then started the same way. With `--target native` the cache holds the linked binary instead, and a second run starts it without rebuilding.

A script exits when it returns, even if it started threads that are still running, as the `kotlin` command does. A library that leaves a non-daemon thread behind, such as an HTTP client's dispatcher, therefore doesn't keep the terminal waiting.

With `--offline`, a script that has run before works from the cache without the network. A script that has never run needs the network to resolve its dependencies and compiler.

:::note
Compiler messages give the line numbers of your file. The path they show is Kargo's copy of the script under the cache directory, because Kargo removes the annotations from that copy before compiling it.
:::

## Run a file with `kargo run`

`kargo run <file>` runs a script the same way: `kargo run fetch.main.kts world` is `kargo script fetch.main.kts world`, and `kargo run -e 'println(1 + 1)'` is `kargo script -e`. A `.kt` file runs the same way too when it has a front matter or lies outside any project.

A `.kt` file without a front matter inside a project is different: `kargo run tools/migrate.kt` compiles it against the project's code and dependencies instead, without adding it to the build. See [Run a single file](../multiple-programs/#run-a-single-file).

## Use a script as a task or hook

Inside a project, a script is the recommended way to write a [task or a hook plugin](../tasks-and-plugins/): it runs the same on Windows, macOS and Linux, so there is no `windows-run` or `.bat` twin to keep. Point `script` at the file in `kargo.toml`:

```toml title="kargo.toml"
[tasks]
gen = { script = "scripts/gen.kts", description = "Generate code", hooks = ["pre-build"] }

[plugins.notify]
script = "scripts/notify.kts"
hooks = ["post-build"]
```

```console
$ kargo task gen
   Running task `gen`
   Compiling `gen.kts`
wrote generated/Version.kt
```

Kargo runs the file as `kargo script` would, with the project root (or the task's `cwd`) as the working directory and the task's or hook's environment: `KARGO_TASK`, `KARGO_HOOK`, `KARGO_PKG_*`, `KARGO_CLASSPATH`, `KARGO_PACKAGE`, the project's `[env]` and `.env` files, and the `env` of the task or plugin. The words after the task name are the script's `args`; a hook plugin's `args` come first, and the hook name is always last. A script that exits with a non-zero code or throws fails the task or the build.

The script still takes its dependencies, Kotlin version and JDK from its own header, not from the project's `kargo.toml`. It compiles on its first run and starts from [the cache](#how-runs-are-cached) after that, so a `pre-build` script costs a JVM startup, not a compile. See [Tasks and plugins](../tasks-and-plugins/#kotlin-scripts) for the keys and a hook example.

## Editors

The [language server](../editors/#language-server) treats a `.kts` file as a Kargo script when it lies outside any project, when its shebang runs `kargo script`, or when the project's `kargo.toml` runs it as the `script` of a task or hook plugin. Any other `.kts` file inside a project is a build script that Kargo does not run, so the server leaves it alone unless its shebang says otherwise. In Zed, a folder with only scripts needs a setting to start the server; see [Kargo scripts in Zed](../editors/#kargo-scripts-in-zed).

For a Kargo script, the server completes the annotation names, the values of `@file:Jdk`, `@file:Framework`, `@file:Repository` and `@file:Target`, the coordinates and versions of `@file:DependsOn`, and the versions of `@file:Kotlin`. It shows what each annotation means on hover, and it reports a header that `kargo script` would reject, such as a dependency without a version, as a diagnostic on that annotation.

The rest of the file goes to the Kotlin server, which reads its top-level statements and `args` as it runs the script. The libraries of its `@file:DependsOn` declarations are loaded for that script alone, so completion, hover and go to definition work on them, while the project's own files don't see them. The [Kotlin server](../kotlin-server/) page explains where those libraries come from.

## Limits

- `@file:Import` is not supported yet. Put the shared code in a `.kt` file in a project, or copy it into the script.
- Compiler plugins do not run in a `.kts` script. `@file:Framework("serialization")`, `"compose"`, `"lombok"` and `"spring-boot"` need a `.kt` file, which runs them the way a project does. `"coroutines"` and `"ktor"` are libraries and work in scripts.
- Native scripts must be `.kt` files with a top-level `fun main`. `.kts` scripts and `-e` code run on the JVM only.
- A native script resolves its dependencies for the host platform, so only klib-based (multiplatform) libraries work. A JVM-only artifact, such as a `-jvm` classifier, does not resolve.
- A `.kts` script's dependencies come from its annotations only. A script has no `kargo.toml`, so it does not use a project's `[dependencies]`, `[toolchain]` or `[env]` settings.
- Each `@file:DependsOn` needs an exact version. Kargo does not resolve the latest one.
- In the editor, the libraries of `@file:Framework` are not loaded, so the classes they add don't complete there yet. `kargo script` runs them.
- Kargo cannot turn a script into a project yet. When a script outgrows one file, create a project with `kargo new` and move the code into it.
