---
title: Several programs in one project
description: Run another main with --main, declare named programs with [[bin]], and keep runnable examples in examples/.
---

A project often holds more than one program: the app, a migration tool, a seed script, a few examples. Kargo has four ways to run them, all through `kargo run`:

| You want to | Use |
|---|---|
| Run some other `main` once | `kargo run --main <class>` |
| Give programs a name and keep them in the build | a `[[bin]]` table and `kargo run --bin <name>` |
| Ship small programs that show how to use your code | `examples/<name>.kt` and `kargo run --example <name>` |
| Run a one-off file kept anywhere in the project | `kargo run <path>.kt` |

## Run another `main`

Say `src/` holds two programs:

```kotlin title="src/Main.kt"
package com.example.app

fun main() = println("the app")
```

```kotlin title="src/tools/Migrate.kt"
package com.example.tools

fun main(args: Array<String>) = println("migrating ${args.joinToString()}")
```

`kargo run` can't pick one, so it lists both:

```console
$ kargo run
   Compiling app v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.12s
error: could not determine the main class: found multiple `main` functions; pick one with `kargo run --main <class>`, declare each as a [[bin]], or set `[target.jvm] main-class` in kargo.toml:
  com.example.app.MainKt (/home/me/app/src/Main.kt)
  com.example.tools.MigrateKt (/home/me/app/src/tools/Migrate.kt)
```

Name the class with `--main`:

```console
$ kargo run --main com.example.tools.MigrateKt -- up
     Running `java com.example.tools.MigrateKt up`
migrating up
```

The class of a top-level `fun main` in `Migrate.kt` is `MigrateKt`; for `object Cli { @JvmStatic fun main(...) }` it is `Cli`.

On [Kotlin/Native](../native/), `--main` takes the entry function instead, and `--entry-point` is an alias that reads better there:

```sh
kargo run -t native --entry-point com.example.tools.main
```

Kargo links that executable separately, into `target/<profile>/native/<platform>/main/`.

To make one of them the default for `kargo run`, set `[target.jvm] main-class` (or `[target.native] entry-point`) in `kargo.toml`.

:::caution
Two top-level `fun main` declarations in the **same package** clash ("conflicting overloads"), even in different files. Give each program its own package, as above.
:::

## Named programs with `[[bin]]`

A `[[bin]]` table gives a program a name, so you don't have to remember its class:

```toml title="kargo.toml"
[target.jvm]
main-class = "com.example.app.MainKt"   # what plain `kargo run` starts

[[bin]]
name = "migrate"
path = "src/tools/Migrate.kt"           # the file with its `main`

[[bin]]
name = "report"
main = "com.example.report.Cli"         # or name the JVM class directly
```

```console
$ kargo run --bin migrate -- down
     Running `java com.example.tools.MigrateKt down`
migrating down
```

Each `[[bin]]` needs a `name` and one way to find its entry point:

| Key | Meaning |
|---|---|
| `name` | What you pass to `--bin`. Letters, digits, `-` and `_`, starting with a letter |
| `path` | A `.kt` file, relative to the project root, that declares exactly one `main`. Works for every target |
| `main` | The JVM main class, e.g. `com.example.tools.MigrateKt` |
| `entry-point` | The Kotlin/Native entry function, e.g. `com.example.tools.main` |

Use `path`, or `main` and/or `entry-point`, not both. A bin with only `main` is JVM-only; one with only `entry-point` is Native-only.

The bin's sources stay in `src/`, so they are compiled with the rest of the project. On the JVM that costs nothing extra: every bin runs from the same classes. With an executable [Native target](../native/), `kargo build` also links one executable per bin with a native entry point, into `target/<profile>/native/<platform>/bin/<name>`. If the sources hold several `main` functions and `entry-point` is not set, the project's own executable is skipped and only the bins are linked.

When `kargo run` can't decide and the project has bins, the error lists them:

```console
$ kargo run
error: could not determine the main class: found multiple `main` functions; ...
run a [[bin]] with `kargo run --bin <name>`; available bins: migrate, report
```

### A jar per bin

`kargo package --bin <name>` builds a fat jar that launches that bin, named after it:

```console
$ kargo package --bin migrate
   Compiling app v0.1.0 (jvm)
   Packaging migrate v0.1.0 (/home/me/app/target/debug/jvm/migrate-0.1.0-all.jar)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.71s
/home/me/app/target/debug/jvm/migrate-0.1.0-all.jar
$ java -jar target/debug/jvm/migrate-0.1.0-all.jar up
migrating up
```

Without `--bin`, `kargo package` launches the project's own main class as before. With `--release`, the `-all.jar` stays and an R8-shrunk `migrate-0.1.0-min.jar` is written next to it; see [`kargo package`](../../reference/commands/#kargo-package).

## Examples

Put small programs that use your code in `examples/`, one file each:

```kotlin title="examples/greet.kt"
import com.example.app.greeting

fun main() = println(greeting("examples"))
```

```console
$ kargo run --example greet
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.21s
   Compiling app v0.1.0 (jvm, example greet)
     Running `java GreetKt`
Hello, examples!
```

Examples work like Cargo's:

- They are **not** part of `kargo build` or the packaged jar. `kargo run --example` compiles the one you ask for.
- Each example is compiled on its own, against the project's classes, its dependencies and its **dev-dependencies**. Several examples can therefore each declare `fun main` in the default package.
- They see your public API only, not `internal` declarations.
- On the JVM, each one goes to `target/<profile>/jvm/examples/<name>/`. On Kotlin/Native, the main sources become a library the example links against, in `target/<profile>/native/<platform>/examples/`. There, an example's `main` must not be in a package where your main sources also declare a `main`; the default package (no `package` line) is the usual choice.
- `kargo fmt` and `kargo lint` check `examples/` too.

## Run a single file

`kargo run` also takes the path of a `.kt` file with a `main`. The file can live anywhere under the project, such as a `tools/` directory next to `src/`:

```kotlin title="tools/migrate.kt"
import com.example.app.Database

fun main(args: Array<String>) {
    Database.migrate(args.firstOrNull() ?: "up")
}
```

```console
$ kargo run tools/migrate.kt down
   Compiling app v0.1.0 (jvm, tools/migrate.kt)
    Finished `dev` profile in 2.1s
     Running `java MigrateKt down`
```

Like an example, the file is compiled on its own against the main classes, the dependencies and the dev-dependencies, into `target/<profile>/jvm/files/<name>/`. It is not part of the project's build, so `kargo build` and `kargo package` leave it out, and an unchanged file is not compiled again. The project is the one the file belongs to, so `kargo run ../app/tools/migrate.kt` works from anywhere.

The first word after `run` is a file only when it ends in `.kt` or `.kts` and exists; every other word is an argument for the program. A `.kts` script, and a `.kt` file with a `/* kargo ... */` front matter, run as [`kargo script`](../scripts/) runs them instead, with their own dependencies.

## Targets

| Target | `--main` | `--bin` | `--example` |
|---|---|---|---|
| JVM | main class | `main`, or the class of the `main` in `path` | yes |
| Native | entry function (`--entry-point`) | `entry-point`, or the top-level `main` in `path` | yes |
| Wasm, Android | no | no | no |

Kotlin/Native only accepts a top-level `fun main`; a `@JvmStatic fun main` in an object is JVM-only.

See also: [`kargo run`](../../reference/commands/#kargo-run-r), [`[[bin]]`](../../reference/manifest/#bin) and [Project layout](../../getting-started/project-layout/).
