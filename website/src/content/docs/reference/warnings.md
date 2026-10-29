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
| [JavaScript modules without types](#javascript-modules-without-types) | `build`, `check`, `run`, `package` |
| [Exported functions without types](#exported-functions-without-types) | `build`, `check`, `run`, `test`, when the declarations of a TypeScript module change |
| [Tasks hidden by built-in commands](#tasks-hidden-by-built-in-commands) | every command that works on a package: `build`, `check`, `run`, `test`, `package`, `doc`, `fmt`, `lint`, `fetch`, `tree` |
| [Odd package versions](#odd-package-versions) | the same commands |
| [npm packages without metadata](#npm-packages-without-metadata) | the same commands |
| [`.env` file tracked by git](#env-file-tracked-by-git) | `run`, `test`, `task`, `env` |
| [Generator inputs that match nothing](#generator-inputs-that-match-nothing) | `build`, `check`, `run`, `test`, `package`, `generate` |

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

## JavaScript modules without types

```text
warning: `js/legacy.js` has no TypeScript types, so Kotlin gets no declarations for it; write a .d.ts file next to it, or make it a .ts file
```

Kargo generates Kotlin declarations for the [JavaScript and TypeScript modules](../../guides/wasm/#your-own-javascript-and-typescript) in `js/` (or `js-sources`) from their TypeScript types. A `.js` or `.mjs` file has none unless a `.d.ts` (or `.d.mts`) file with the same name sits next to it, so Kotlin can only call it through `@JsModule` declarations you write yourself. Write the `.d.ts` file, or rename the module to `.ts` and add the types. Kargo names the first five modules and counts the rest. The Wasm and JS targets are checked, the JS target from Kotlin 2.2.20 on.

## Exported functions without types

```text
warning: `add` in js/math.ts has a parameter or return type without an annotation, so Kotlin sees `JsAny?` there; write the types of exported functions out
```

Kargo reads the signatures of a TypeScript module's exports, not their bodies, so it can't infer what tsc would: a parameter or return type left to inference is `JsAny?` in the Kotlin declarations. Annotate the exported function (`export function add(a: number, b: number): number`). The warning is printed when the module's declarations change, not on every build.

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

## npm packages without metadata

```text
warning: the npm package has no license, no repository; registries show them on the package's page, so set them in [package]
warning: [publish.npm] is ignored: no npm package is built; set `kind = "library"` under [target.wasm] or [target.js] to publish one
```

A package with a [`[publish.npm]`](../manifest/#publishnpm) table publishes a Wasm or Kotlin/JS library to npm with [`kargo publish --npm`](../commands/#kargo-publish). npm accepts a package without a `description`, `license` or `repository`, but the registry's page for it then shows none, and tools that check licenses can't tell what it is. Set them under [`[package]`](../manifest/#package); they are written to `package.json` (the license as an SPDX expression, the repository as `git+https://...git`). Without `[publish.npm]` the first warning is not printed.

The second warning means the table has nothing to publish: only a `kind = "library"` target of `[target.wasm]` (with `environment = "js"` or `"browser"`, not `"wasi"`) or `[target.js]` is an npm package. An application is not.

## `.env` file tracked by git

```text
warning: `.env` is tracked by git but sets API_TOKEN; keep secrets in a git-ignored `.local` file or in your shell
```

A `.env` file that git tracks is shared with everyone who clones the repository. Kargo warns when such a file sets a variable whose name looks like a secret (it contains `SECRET`, `TOKEN`, `PASSWORD`, `PASSWD` or `CREDENTIAL`, or it is `KEY` or ends in `_KEY`) with a value. Move the secret into `.env.local` or `.env.<profile>.local`, which `kargo new` git-ignores, or into your shell, and remove the file from git with `git rm --cached .env`. Empty values don't count. Without git, or for files git doesn't track, there is no warning.

## Generator inputs that match nothing

```text
warning: no file matches the `inputs` of [[generate]] `protos`: proto/**/*.proto
```

A [`[[generate]]` step](../../guides/code-generation/) with `inputs` runs when those files change, and none of the globs matches a file. Usually the glob has a typo or the files are somewhere else. Globs are relative to the project root, use gitignore syntax, and never match inside `target/` or a directory whose name starts with a dot. A `protobuf` step with no `.proto` file generates nothing; any other step still runs.

## Odd package versions

```text
warning: package version `not.a.version.x` does not look like a version (expected something like `1.0.0`); it becomes part of the jar's file name and of its Maven coordinates
```

[`[package] version`](../manifest/#package) names the jar (`app-1.0.0.jar`) and is the version in its Maven coordinates. Maven accepts almost any string, so Kargo does too, apart from [ranges, wildcards and spaces](../manifest/#package), which are errors. But a version that doesn't start with a digit, or uses anything but letters, digits and `.`, `-`, `_` and `+`, is rarely intended. `0.1.0`, `1.0-SNAPSHOT`, `2.1.0-rc.1` and `2024.03` are fine; `v1.0`, `latest` and `1/2` are not.

## Stale member lockfile

```text
warning: package `app`: `app/kargo.lock` is ignored: the package is locked in the workspace's `kargo.lock`; delete the stale file
```

A [workspace](../../guides/workspaces/) locks all its members in one `kargo.lock` at its root. A member that was a standalone project before keeps its old `kargo.lock`, which Kargo no longer reads or writes. Delete it; running `kargo fetch` at the workspace root pins the member in the shared lockfile.

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
