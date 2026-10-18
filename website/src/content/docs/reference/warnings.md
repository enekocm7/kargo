---
title: Warnings
description: The warnings Kargo prints about projects that build but are probably not what you meant, and how to fix each one.
---

Some project states are valid, so Kargo builds them, yet they are almost always mistakes: a project with no sources, a script that is never compiled, a task you can't run by its name. Kargo prints a `warning:` for each one on stderr and carries on.

```console
$ kargo build
warning: package version `not.a.version.x` does not look like a version (expected something like `1.0.0`); it becomes part of the jar's file name and of its Maven coordinates
warning: task `build` is hidden by the built-in `kargo build` command; run the task with `kargo task build` or rename it
warning: no Kotlin or Java sources found in src/ or src-jvm/ for the jvm target
warning: `src/script.kts` is ignored: Kargo does not compile or run Kotlin scripts (.kts); put the code in a .kt file, or run it with `kargo script <file>`
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.17s
```

Warnings never change the exit code, and `-q` hides them. In a [workspace](../../guides/workspaces/) each one starts with ``package `name`:`` so you can tell which member it is about.

| Warning | Printed by |
|---|---|
| [No sources](#no-sources) | `build`, `check`, `run`, `package` |
| [Ignored Kotlin scripts](#ignored-kotlin-scripts) | `build`, `check`, `run`, `package`, `test` |
| [Tasks hidden by built-in commands](#tasks-hidden-by-built-in-commands) | every command that works on a package: `build`, `check`, `run`, `test`, `package`, `doc`, `fmt`, `lint`, `fetch`, `tree` |
| [Odd package versions](#odd-package-versions) | the same commands |
| [`.env` file tracked by git](#env-file-tracked-by-git) | `run`, `test`, `task`, `env` |

## No sources

```text
warning: no Kotlin or Java sources found in src/ or src-jvm/ for the jvm target
```

A target with nothing to compile still "builds": there is nothing to fail, so `kargo build` prints `Finished`. The mistake shows up later, for example as `could not determine the main class` from `kargo run`. Kargo checks each target the command builds. It needs a `.kt` file in `src/` or `src-<target>/` (a `.kt` or `.java` file on the JVM).

- The check runs after the `pre-build` [hooks](../../guides/tasks-and-plugins/#hooks), so sources a hook generates count.
- A JVM project with files in `resources/` and no sources is a valid resources-only jar and gets no warning.
- Android is not checked here; Gradle reports an Android project without sources itself.
- `kargo test` doesn't warn: a project can test code that lives elsewhere.

## Ignored Kotlin scripts

```text
warning: `src/script.kts` is ignored: Kargo does not compile or run Kotlin scripts (.kts); put the code in a .kt file, or run it with `kargo script <file>`
```

Kargo compiles `.kt` and `.java` files. A `.kts` file in `src/`, `src-<target>/`, `tests/`, `tests-<target>/` or `examples/` is not compiled and not run, so its code silently does nothing. Move the code into a `.kt` file (a program needs a `fun main`), or delete the file if it is a Gradle build script left over from a [migration](../../migration/from-gradle/). To run a script on its own, without a project, use [`kargo script`](../../guides/scripts/).

Kargo names the first five scripts and counts the rest. A `.kts` file under `resources/` or `test-resources/` is data and is not reported.

## Tasks hidden by built-in commands

```text
warning: task `build` is hidden by the built-in `kargo build` command; run the task with `kargo task build` or rename it
```

`kargo <name>` runs a [task](../../guides/tasks-and-plugins/) only when `<name>` is not a built-in command, so `kargo build` always compiles the project, whatever `[tasks]` says. The task itself still works: run it with `kargo task build`, or let it run as a [hook](../../guides/tasks-and-plugins/#hooks). Aliases count too, so a task named `b`, `c`, `r` or `t` is hidden by `kargo build`, `check`, `run` or `test`.

`kargo task` and `kargo --help` mark hidden tasks:

```console
$ kargo task
Available tasks:
    build  echo custom build [hidden by `kargo build`; run it with `kargo task build`]
    ci     echo ci
```

Rename the task to make `kargo <name>` work as a shorthand again.

## `.env` file tracked by git

```text
warning: `.env` is tracked by git but sets API_TOKEN; keep secrets in a git-ignored `.local` file or in your shell
```

A `.env` file that git tracks is shared with everyone who clones the repository. Kargo warns when such a file sets a variable whose name looks like a secret (it contains `SECRET`, `TOKEN`, `PASSWORD`, `PASSWD` or `CREDENTIAL`, or it is `KEY` or ends in `_KEY`) with a value. Move the secret into `.env.local` or `.env.<profile>.local`, which `kargo new` git-ignores, or into your shell, and remove the file from git with `git rm --cached .env`. Empty values don't count. Without git, or for files git doesn't track, there is no warning.

## Odd package versions

```text
warning: package version `not.a.version.x` does not look like a version (expected something like `1.0.0`); it becomes part of the jar's file name and of its Maven coordinates
```

[`[package] version`](../manifest/#package) names the jar (`app-1.0.0.jar`) and is the version in its Maven coordinates. Maven accepts any string, so Kargo does too, but a version that doesn't start with a digit, or uses anything but letters, digits and `.`, `-`, `_` and `+`, is rarely intended. `0.1.0`, `1.0-SNAPSHOT`, `2.1.0-rc.1` and `2024.03` are fine; `v1.0`, `latest` and `1.0 beta` are not.

## Warnings as errors

With [`[kotlin] warnings-as-errors = true`](../manifest/#kotlin) the Kotlin compiler reports its warnings as errors. It prints a summary of that, `error: warnings found and -Werror specified`, ahead of the diagnostics it counts. Kargo holds the line back and prints it after them, as the last line of the compiler's output, so the diagnostics are never announced before you can see them:

```console
$ kargo build
   Compiling app v0.1.0 (jvm)
src/Main.kt:9:12: error: 'fun old(): Int' is deprecated. use other.
    return old()
           ^^^
error: warnings found and -Werror specified
error: could not compile `app` due to previous errors
```

With `--message-format json` it is the last `compiler-message`. See [Machine-readable output](../machine-readable-output/#compiler-diagnostics).
