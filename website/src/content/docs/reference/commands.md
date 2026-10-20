---
title: Commands
description: Every kargo command and flag.
---

Run `kargo help <command>` for the same information in your terminal.

## Help

`kargo --help` (also `-h` or `kargo help`) lists the common commands, then the tasks of the project you are in, and ends with a pointer to `kargo --list`. `kargo` with no arguments prints the same help and exits with 0. `kargo --list` prints every built-in command with its description, the way `cargo --list` does:

```console
$ kargo --list
Built-in commands:
    new          Create a new Kotlin project in a new directory
    init         Create a new Kotlin project in an existing directory
    ...
    lsp          Run the language server for editors (LSP over stdin and stdout)
    dap          Run the debug adapter for editors (DAP over stdin and stdout)
    info         Print project information
    completions  Print a shell completion script
```

The commands the short help leaves out (`outdated`, `upgrade`, `target`, `info`, `plugins`, `android`, `daemon`, `metadata`, `classpath`, `ide`, `lsp`, `dap`, `self` and `completions`) work as usual, and `kargo help <command>` shows each one's options. Shell completions offer them too.

A command that doesn't exist suggests the closest built-in command, task or `kargo-*` plugin, as Cargo does:

```console
$ kargo biuld
error: no such command: `biuld`

	Did you mean `build`?

	View all installed commands with `kargo --help` and `kargo plugins`
```

## Global flags

These work with every command.

| Flag | Meaning |
|---|---|
| `-v`, `--verbose` | Show the compiler and tool commands Kargo runs |
| `-q`, `--quiet` | Don't print Kargo's status messages |
| `--offline` | Never use the network (also `KARGO_OFFLINE=1`) |
| `--locked` | Fail if `kargo.lock` would need to change, and hash every cached artifact instead of trusting its `.stamp` |
| `--vendored` | Resolve dependencies only from the repository [`kargo vendor`](#kargo-vendor) wrote (also `KARGO_VENDORED=1`, or `[source] vendored = true` in `.kargo/config.toml`) |
| `--refresh` | Ignore cached metadata and `kargo.lock`, re-check repositories, and re-verify cached artifacts (re-downloading corrupt ones) |
| `--system-gradle` | Run Android builds with the `gradle` on `PATH` instead of a Kargo-managed Gradle (also `KARGO_SYSTEM_GRADLE=1`). A project `gradlew` still wins |
| `--manifest-path <PATH>` | Use this `kargo.toml` instead of searching from the current directory |
| `-V`, `--version` | Print the version. With `-v` (`kargo -vV`) also the release and the target it was built for (`host: x86_64-unknown-linux-gnu`). When a newer release exists, a note on stderr says so, at most once a day (see [`kargo self`](#kargo-self)) |
| `-h`, `--help` | Print help |

**Exit codes:** `0` on success, `101` when a build or test fails or Kargo reports an error (as in Cargo). `kargo run` exits with your program's exit code.

Targets are `jvm`, `wasm`, `native` and `android`.

## Package selection

In a [workspace](../../guides/workspaces/), commands apply to the default members at the workspace root (every member, unless [`default-members`](../manifest/#workspace) says otherwise) and to the current member inside a member's directory. These options pick packages explicitly:

| Option | Commands | Meaning |
|---|---|---|
| `-p, --package <name>` | `build`, `check`, `test`, `doc`, `fmt`, `lint`, `tree`, `fetch`, `publish` | Only this package (repeatable) |
| `--workspace` | the same | Every member of the workspace |
| `--exclude <name>` | the same | Leave this member out (repeatable), with `--workspace` or at the workspace root |
| `-p, --package <name>` | `run`, `package`, `install`, `add`, `remove`, `outdated`, `upgrade` | The one package to work on; required at the root of a workspace with several default members |

`-p` takes any package of the project, including a path dependency that is not a member. Outside a workspace there is one package, so you never need these options.

### Concurrent commands

Commands that build or resolve a project (`build`, `check`, `run`, `test`, `doc`, `fmt`, `lint`, `tree`, `classpath`, `fetch`, `metadata`, `ide`, `package`, `publish`, `install --path`/`--git`, `update`, `clean`, `android`, `toolchain which`, and `lsp` while it imports the project) take an exclusive lock on `target/.kargo-lock` first. That lock guards both `target/` and `kargo.lock`. If another Kargo process (a second terminal, an IDE or a file watcher) holds the lock, the command waits for it to finish:

```console
$ kargo build
    Blocking waiting for file lock on build directory
   Compiling my-app v0.1.0 (jvm)
```

`kargo run` releases the lock once the build is done, so a long-running program doesn't block other commands. Downloads to the shared cache in `~/.kargo` don't need the lock: each file is written to a temporary name and renamed into place, so two projects can fetch the same artifact at the same time.

---

## `kargo new`

Create a project in a new directory.

```sh
kargo new <path> [options]
```

| Option | Default | Meaning |
|---|---|---|
| `--lib` | | Library template instead of an application |
| `-t, --target <list>` | `jvm` | Comma-separated targets to enable |
| `-w, --with <list>` | | Frameworks: `compose`, `serialization`, `coroutines`, `ktor`, `lombok`, `spring-boot`, `benchmark` |
| `--bench` | | Add the `benchmark` framework and a sample benchmark in `benches/SampleBench.kt`, as `--with benchmark` does. See [Benchmarking](../../guides/benchmarking/) |
| `--group <group>` | `com.example` | Maven group, also the base Kotlin package |
| `--name <name>` | directory name | Package name |
| `--kotlin <version>` | `2.4.21` | Kotlin version |
| `--no-vcs` | | Don't create a git repository |
| `--workspace` | | Create an empty [workspace](../../guides/workspaces/#creating-a-workspace) instead of a package |

```sh
kargo new hello
kargo new mylib --lib -t jvm,wasm,native
kargo new api -w ktor,serialization --group io.acme
kargo new shop --workspace
```

Inside a workspace, `kargo new` (and `kargo init`) adds the new package to the workspace's `members`, unless a `members` glob already covers it, and the package shares the workspace's git repository. `--workspace` writes a `kargo.toml` with an empty `[workspace]` and a `[workspace.kotlin]` that pins the Kotlin version, and a `.gitignore`.

The generated `kargo.toml` stays minimal: a `[package]` section, the `[target.*]` sections for any non-JVM targets (plus an empty `[target.jvm]` when JVM is one of several targets) and `[frameworks]` when you pass `--with`. It has no `[kotlin]` section unless `--kotlin` picks a non-default version, so the project uses Kargo's default Kotlin version and JVM target. Add `[dependencies]`, `[dev-dependencies]` and `[tasks]` when you need them; `kargo add` creates the dependency sections for you.

The generated code lives in the package `<group>.<name>`, with the name lowercased and every character other than a letter or digit replaced by `_` (`my-app` becomes `com.example.my_app`). A segment that is a Kotlin or Java keyword gets a `_` suffix, so `kargo new class` uses `com.example.class_`.

Kargo runs `git init` in the new directory when `git` is on the `PATH`, unless you pass `--no-vcs` or the directory is already inside a git repository (a monorepo or an `examples/` folder), in which case the project joins that repository instead of nesting a new one. A `.gitignore` is written either way, unless one already exists.

## `kargo init`

Same as `kargo new`, but in an existing directory (the current one by default). Existing files are not overwritten. A directory that already has a `kargo.toml` is refused; use [`kargo target add`](#kargo-target) to enable more targets in it.

```sh
kargo init [path] [options]
```

## `kargo build` (`b`)

Compile all enabled targets, or the ones you name. Targets build at the same time; if one fails, the command reports that failure and exits with code 101.

| Option | Meaning |
|---|---|
| `--keep-going` | Build every target even if one fails, then report each failure and which targets failed and succeeded |
| `--xcframework` | Also combine the Apple frameworks of `kind = "framework"` into `target/<profile>/native/<name>.xcframework` (macOS with Xcode). See [C interop](../../guides/c-interop/#calling-kotlin-from-swift-and-objective-c) |
| `--message-format <fmt>` | `human` (default) or `json`: print each compiler diagnostic as a JSON object on stdout. See [Machine-readable output](../machine-readable-output/#compiler-diagnostics) |
| `-r, --release` | Use the release profile |
| `--profile <name>` | Use a custom profile from kargo.toml, such as `staging`, instead of `dev` (see [Profiles](../manifest/#profilename)) |
| `-t, --target <target>` | Only this target (repeatable) |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | Which packages to build ([package selection](#package-selection)) |
| `-j, --jobs <n>` | How many workspace members build at the same time (default: the number of CPUs) |

[Path dependencies](../../guides/workspaces/) are built first, for the same targets. In a workspace, members build in dependency order, and independent members at the same time ([the build graph](../../guides/workspaces/#the-build-graph)); after a failure no new member starts, and the error names the member and the target, such as ``could not build `core` (jvm)``.

Before compiling, `kargo build` (like `check`, `run`, `test` and `package`) [warns](../warnings/) about projects that are valid but probably not what you meant: a target with no sources, Kotlin scripts (`.kts`) that are never compiled, a [task](../../guides/tasks-and-plugins/) hidden by a built-in command, and an odd package version.

For Wasm, a release build removes dead code, runs `wasm-opt` when it is installed and prints the size of the module. A `kind = "library"` project also gets a `package.json` and TypeScript declarations. See [WebAssembly](../../guides/wasm/).

```sh
kargo build
kargo build --release -t jvm -t native
kargo build --profile staging
kargo build -p calc
kargo build --message-format json
```

With `--keep-going`, every failed target gets its own error and a summary follows:

```console
$ kargo build --keep-going
error: could not build the wasm target: ...
error: could not build 1 of 3 targets (wasm); jvm, native succeeded
```

## `kargo check` (`c`)

Type-check all enabled targets, or the ones you name, and report compiler errors and warnings without packaging or linking. It is the fastest feedback loop for editors and CI lint stages.

| Option | Meaning |
|---|---|
| `--tests` | Also check the test sources (`tests/`, `tests-<target>/`) |
| `--keep-going` | Check every target even if one fails, then report each failure (as for `kargo build`) |
| `--message-format <fmt>` | `human` (default) or `json`, as for `kargo build` |
| `-r, --release` | Use the release profile |
| `--profile <name>` | Use a custom profile from kargo.toml, such as `staging`, instead of `dev` (see [Profiles](../manifest/#profilename)) |
| `-t, --target <target>` | Only this target (repeatable) |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | Which packages to check |
| `-j, --jobs <n>` | How many workspace members are checked at the same time (default: the number of CPUs) |

```sh
kargo check
kargo check --tests -t native
```

What each target skips:

| Target | `kargo check` does | Output |
|---|---|---|
| JVM | Compiles Kotlin and Java, but writes no jar and no `lib/`. Kotlin has no analysis-only mode on the JVM, so the classes go to the same place as `kargo build`'s, and a later build reuses them. | `target/<profile>/jvm/classes` |
| Wasm | Produces only the klib; skips linking the `.mjs`/`.wasm` module | `target/<profile>/check/wasm` |
| Native | Produces a klib for each platform; skips code generation and linking | `target/<profile>/check/native/<platform>` |
| Android | Runs Gradle's `compileDebugSources` (`compileReleaseSources` with `--release`), plus `compileDebugUnitTestSources` with `--tests`. Skips dexing and APK/AAR packaging | `target/android/build` |

`pre-build` [hooks](../../guides/tasks-and-plugins/#hooks) run first, so generated sources are checked too. `post-build` hooks don't run. `[kotlin] warnings-as-errors`, `--locked` and `--offline` behave as they do for `kargo build`, and a failed check exits with code 101.

## `kargo run` (`r`)

Build and run the program. Arguments after `--` go to the program.

| Option | Meaning |
|---|---|
| `-r, --release` | Use the release profile |
| `--profile <name>` | Use a custom profile from kargo.toml, such as `staging`, instead of `dev` (see [Profiles](../manifest/#profilename)) |
| `-t, --target <target>` | Target to run. Default: the first enabled of `jvm`, `native`, `wasm`, `android` |
| `-p, --package <name>` | The package to run; required at the root of a workspace with several members |
| `--main <name>` | Run another `main`: a JVM class such as `com.example.tools.MigrateKt`, or on Native an entry function such as `com.example.tools.main`. `--entry-point` is an alias |
| `--bin <name>` | Run the [`[[bin]]`](../manifest/#bin) with this name |
| `--example <name>` | Compile and run `examples/<name>.kt` against the project and its dev-dependencies |
| `--port <port>` | Wasm browser apps: the port to serve on (default `8080`; `0` picks a free one) |
| `--open` | Wasm browser apps: open the served page in your browser |
| `--debug[=<port>]` | JVM: start the program suspended until a debugger attaches over JDWP to `127.0.0.1:<port>` (default `5005`; `0` lets the JVM pick and print one). See [Debugging](../../guides/editors/#debugging) |
| `--native-agent` | JVM: run on GraalVM (`[toolchain] graalvm`) with its tracing agent, which records the reflection, resources and proxies the program uses as `native-image` configuration in `native-image/`. Later runs add to it. See [Native executables](../../guides/distributing/#native-executables) |
| `--env-file <path>` | Read this `.env` file after the standard ones, so its values win. Repeatable. Conflicts with `--no-env-file`. See [Environment variables](../../guides/environment/#choose-the-files-for-one-command) |
| `--no-env-file` | Read none of the `.env` files; the variables in kargo.toml and the shell still apply |
| `--flavor <name>` | Android: install this [product flavor](../../guides/android/#product-flavors) (repeatable, one per dimension; default: the first flavor of each dimension). Needs `-t android` |
| `--message-format <fmt>` | `human` (default) or `json` for the compiler diagnostics, as for `kargo build`. The program's own output still goes to stdout |
| `--watch` | Rebuild and restart the program when its files change, until you press Ctrl+C. A failed build prints its errors and leaves the running program alone. Conflicts with `--debug`; not for Android or Wasm browser apps. See [Watch mode](../../guides/watch/) |
| `--clear` | With `--watch`: clear the terminal before each run |
| `--ignore <glob>` | With `--watch`: leave out the files matching this gitignore-style pattern, on top of `.gitignore`. Repeatable |

`--clear` and `--ignore` need `--watch`.

`--main`, `--bin` and `--example` exclude each other and work for the JVM and Native targets. Without them, `kargo run` starts `main-class` / `entry-point` or the single `main` in the sources; with several, it lists them and any bins. See [Several programs in one project](../../guides/multiple-programs/).

The `Running` line quotes arguments that contain spaces or shell characters, so you can copy it: ``Running `java com.example.MainKt add "buy milk"` `` shows that `buy milk` is one argument.

`--port` and `--open` only apply to a Wasm browser app; `kargo run` rejects them for any other target. A Wasm project with `kind = "library"` has no program, so `kargo run` rejects it too.

How each target runs:

| Target | Runs |
|---|---|
| JVM | `java -cp ... <main-class>` with `jvm-args` |
| Wasm | `node target/<profile>/wasm/<name>.mjs`; the arguments reach `main(args)` (see [Program arguments](../../guides/wasm/#program-arguments)). With `environment = "browser"` it serves `target/<profile>/wasm/` on `127.0.0.1` until you press Ctrl+C (see [Browser apps](../../guides/wasm/#browser-apps)) |
| Native | the executable built for the host platform |
| Android | installs on the connected device with Gradle, then launches it with `adb` |

## `kargo script`

Run a single Kotlin file without a project. A `.kts` script takes its dependencies, repositories, Kotlin version, JDK and frameworks from `@file:` annotations at its top; a `.kt` file takes them from a `/* kargo ... */` front matter. See [Kotlin scripts](../../guides/scripts/).

```sh
kargo script fetch.kts https://example.com
kargo script -e 'println(1 + 1)'
```

| Option | Meaning |
|---|---|
| `<file>` | The `.kts` script or `.kt` file to run. Required unless `-e` is given |
| `-e, --expr <code>` | Run this Kotlin code instead of a file. Every positional argument is then one of the script's arguments |
| `[args]...` | Arguments for the script. Use `--` before an argument that starts with `-` |

The global flags apply: `--offline` runs from the cache alone, `-v` shows the compiler commands, `-q` hides Kargo's status lines.

Exit codes: the script's own exit code when it runs; `1` when the script throws an exception; `101` when the header is invalid or the file does not compile; `2` for a usage error, such as no file. Compiled scripts are cached in `~/.kargo/cache/scripts`.

## `kargo test` (`t`)

Build and run tests.

```sh
kargo test [filter] [options]
```

| Option | Meaning |
|---|---|
| `filter` | Only run tests whose `package.Class.method` name contains this string, for example `MathTest` or `MathTest.adds`. Ignored on Wasm. See [Filtering tests](../../guides/testing/#filtering-tests). |
| `--no-run` | Compile tests but don't run them |
| `--no-fail-fast` | Test every target even if one fails, then print a result per target. Overrides [`[test] fail-fast`](../manifest/#test) |
| `--include-tag <tag>` | Only run JVM tests with this JUnit `@Tag`. Repeatable. See [Tags](../../guides/testing/#tags) |
| `--exclude-tag <tag>` | Skip JVM tests with this JUnit `@Tag`. Repeatable. See [Tags](../../guides/testing/#tags) |
| `--failed` | Rerun only the JVM test classes that failed in the last run. Conflicts with `--list` and `--no-run`. See [Rerunning failures](../../guides/testing/#rerunning-failures) |
| `--report junit` | Also write JUnit XML reports to `target/<profile>/<target>/test-reports/` for CI systems. See [Test reports for CI](../../guides/testing/#test-reports-for-ci) |
| `--debug[=<port>]` | Start the JVM tests suspended until a debugger attaches over JDWP to `127.0.0.1:<port>` (default `5005`). Needs `-t jvm` in a project with several targets. See [Debugging tests](../../guides/testing/#debugging-tests) |
| `--list` | Compile the tests and print them (`<target>: <Class>.<name>`) instead of running them. JVM and Native only. See [Listing tests](../../guides/testing/#listing-tests) |
| `--format <fmt>` | With `--list`: `text` (default) or `json`, one [`test-discovered`](../machine-readable-output/#test-discovery) object per test |
| `--message-format <fmt>` | `human` (default) or `json`: compiler diagnostics and each test's start and result as [JSON messages](../machine-readable-output/#test-results) on stdout; the test report moves to stderr |
| `-r, --release` | Use the release profile |
| `--profile <name>` | Use a custom profile from kargo.toml, such as `staging`, instead of `dev` (see [Profiles](../manifest/#profilename)) |
| `-t, --target <target>` | Only this target (repeatable) |
| `--env-file <path>` | Read this `.env` file after the standard ones, so its values win. Repeatable. Conflicts with `--no-env-file`. See [Environment variables](../../guides/environment/#choose-the-files-for-one-command) |
| `--no-env-file` | Read none of the `.env` files; the variables in kargo.toml and the shell still apply |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | Which packages to test, in dependency order. The command fails if any test failed |
| `-j, --jobs <n>` | How many packages are tested at the same time (default 1, so test output doesn't interleave) |
| `--watch` | Run the tests again whenever the project's files change, until you press Ctrl+C. A failing test does not stop watching. Conflicts with `--list` and `--debug`. See [Watch mode](../../guides/watch/#rerun-tests-on-change) |
| `--clear`, `--ignore <glob>` | With `--watch`, as for [`kargo run`](#kargo-run-r) |

By default `kargo test` stops at the first target whose tests fail or don't compile, and notes which targets it skipped. See [Testing every target](../../guides/testing/#testing-every-target).

Wasm tests run on Node.js, or with `environment = "browser"` in a headless browser: Chrome, Edge, Chromium, Brave or Firefox, found on `PATH` or named by `KARGO_BROWSER`. See [Testing in a browser](../../guides/wasm/#testing-in-a-browser).

## `kargo bench`

Build and run the benchmarks of the JVM target with [JMH](https://github.com/openjdk/jmh), and print the results.

```sh
kargo bench [filter] [options]
```

| Option | Meaning |
|---|---|
| `filter` | Only run benchmarks whose `package.Class.method` name contains this string, for example `ParseBench` or `ParseBench.parse` (`#` also separates the method) |
| `--no-run` | Compile the benchmarks and generate JMH's harness but don't run them. Conflicts with `--save` and `--baseline` |
| `--save <name>` | Store the results as the baseline `<name>` in `target/bench/<name>.json` (`target/bench/<member>/` in a workspace), replacing one of that name |
| `--baseline <name>` | Compare the results with the baseline `<name>`: each benchmark shows its old and new score, the change in percent and whether Welch's t-test finds it significant (`p < 0.05`). The command fails before building when the baseline doesn't exist |
| `--format <fmt>` | `text` (default) prints the results as a table, or the comparison with `--baseline`. `json` prints JMH's JSON results on stdout and moves the report to stderr |
| `--warmup <NxT>` | Warmup iterations, such as `3x1s` or `5x500ms`. Overrides [`[bench] warmup`](../manifest/#bench) |
| `--iterations <NxT>` | Measured iterations, in the same format. Overrides `[bench] iterations` |
| `--forks <n>` | How many JVMs run each benchmark; `0` runs it in the JVM of the launcher. Overrides `[bench] forks` |
| `--mode <mode>` | `thrpt`, `avgt`, `sample`, `ss` or `all`. Overrides `[bench] mode` |
| `--time-unit <unit>` | `ns`, `us`, `ms`, `s`, `m`, `h` or `d`. Overrides `[bench] time-unit` |
| `--message-format <fmt>` | `human` (default) or `json`: compiler diagnostics as [JSON messages](../machine-readable-output/) on stdout |
| `-r, --release` | Use the release profile (the default, unless `kargo.toml` defines `[profile.bench]` in every selected package) |
| `--profile <name>` | Use a custom profile from kargo.toml, such as `bench` (see [Profiles](../manifest/#profilename)) |
| `-t, --target <target>` | Only this target. Benchmarks run on the JVM only, so any other target is an error |
| `--env-file <path>`, `--no-env-file` | As for [`kargo test`](#kargo-test-t) |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | Which packages to benchmark, in dependency order |
| `-j, --jobs <n>` | How many packages are built at the same time (default 1, so benchmarks don't compete for the machine) |

Benchmarks are kept in `benches/` and `benches-jvm/`, use kotlinx-benchmark's annotations, and are not part of `build`, `test`, `package` or `publish`. A benchmark that can't run (an exception, or a filter that matches nothing) makes `kargo bench` exit with `101`; a benchmark that got slower doesn't. See [Benchmarking](../../guides/benchmarking/).

## `kargo watch`

Run a kargo command again whenever the project's files change. See [Watch mode](../../guides/watch/).

```sh
kargo watch                          # kargo build on every change
kargo watch check                    # kargo check on every change
kargo watch -x "test -t jvm" -x run  # two commands, in order
```

| Option | Meaning |
|---|---|
| `[command]...` | The command to run on each change, with its arguments, such as `check` or `test -t jvm`. Default: `build`. Conflicts with `-x` |
| `-x, --exec <command>` | A command line to run on each change, split on spaces (quotes are not interpreted). Repeatable. The commands run in order, and the ones after a failing command wait for the next change |
| `--clear` | Clear the terminal before each run |
| `--ignore <glob>` | Leave out the files matching this gitignore-style pattern, on top of `.gitignore`. Repeatable |

Each command runs as its own `kargo` process, with the global flags given to `kargo watch` (`-v`, `-q`, `--offline`, `--locked`, `--refresh`, `--system-gradle`, `--manifest-path`). Put the watch options before the command, as in `kargo watch --clear test`.

`kargo watch` waits for each command to finish before it watches again, so a command that keeps running, such as `run`, keeps it from seeing changes. To restart a program on change, use [`kargo run --watch`](#kargo-run-r). A failing command does not stop `kargo watch`; it only exits with an error when it cannot start, for example outside a project.

## `kargo doc` (`d`)

Generate HTML API documentation from KDoc comments with Dokka, into `target/debug/doc/`.

```sh
kargo doc [--open] [-t <target>]
```

| Option | Meaning |
|---|---|
| `--open` | Open the documentation in a browser when it is generated (the browser in `BROWSER`, else the system default) |
| `-t, --target <target>` | Only this target (repeatable); defaults to every enabled target |
| `-p, --package <name>`, `--workspace` | Which packages to document. In a workspace, each goes to `target/debug/<name>/doc/`, and `--open` opens the first |

Configure it with [`[doc]`](../manifest/#doc). See [API documentation](../../guides/documentation/).

## `kargo fmt`

Format the Kotlin files in `src/`, `src-<target>/`, `tests/` and `tests-<target>/` with ktlint.

```sh
kargo fmt [--check]
```

| Option | Meaning |
|---|---|
| `--check` | Report unformatted code without changing files; exit with code 101 if there is any |
| `-p, --package <name>`, `--workspace` | Which packages to format |

Without `--check`, files are rewritten in place, and the command exits with code 101 if ktlint found problems it can't fix. ktlint reads `.editorconfig`; configure the rest with [`[fmt]`](../manifest/#fmt). See [Formatting and linting](../../guides/formatting-and-linting/).

## `kargo lint`

Analyze the Kotlin files in `src/`, `src-<target>/`, `tests/` and `tests-<target>/` with detekt. Exits with code 101 if detekt reports issues.

```sh
kargo lint [--create-baseline]
```

| Option | Meaning |
|---|---|
| `--create-baseline` | Record the current issues in the baseline file (`detekt-baseline.xml` by default) instead of reporting them |
| `-p, --package <name>`, `--workspace` | Which packages to analyze |

detekt reads `detekt.yml`; configure the rest with [`[lint]`](../manifest/#lint). See [Formatting and linting](../../guides/formatting-and-linting/).

## `kargo add`

Add dependencies to `kargo.toml`, enable frameworks, or add a C library.

```sh
kargo add <group:artifact[:version[:classifier]]>... [options]
kargo add --path <dir> [--dev] [-t <target>] [--scope <scope>]
kargo add -F <framework,...>
kargo add --cinterop <name> [--header <file>]... [--pkg-config <package>]
```

| Option | Meaning |
|---|---|
| `--path <dir>` | Depend on the Kargo package in `<dir>` (relative to the current directory), built from source. The entry is named after that package, and its path is written relative to `kargo.toml`. See [path dependencies](../../guides/workspaces/) |
| `--dev` | Add to `[dev-dependencies]` |
| `-t, --target <target>` | Add to `[target.<target>.dependencies]` |
| `--scope <scope>` | `compile`, `runtime`, `provided`, `processor` (javac annotation processor) or `ksp` (KSP processor) |
| `--bom` | Import as a BOM |
| `--pre` | Allow pre-release versions when picking the latest |
| `--no-verify` | Don't check that the artifact exists |
| `-F, --framework <list>` | Enable built-in frameworks instead |
| `--cinterop <name>` | Add the C library `<name>` instead: a `[target.native.cinterop.<name>]` entry (with `package = "<name>"`) and, unless it exists, `interop/<name>.def`. Needs `[target.native]`. See [C interop](../../guides/c-interop/) |
| `--header <file>` | With `--cinterop`: a header the `.def` file binds (repeatable; default `<name>.h`) |
| `--pkg-config <package>` | With `--cinterop`: the pkg-config package that gives the library's flags |
| `-p, --package <name>` | The workspace member whose `kargo.toml` to edit |

`kargo add --path` refuses a dependency that would form a cycle, reuse another package's name or bring a second Kotlin version, and leaves `kargo.toml` unchanged.

Without a version, the latest release is used. Plain versions win over variants that some libraries publish next to them, such as `0.8.0-0.6.x-compat` or `33.0.0-jre`; Kargo prints a note naming the variant it skipped. When a library publishes only variants, the newest one is used.

## `kargo remove` (`rm`)

Remove dependencies from `kargo.toml`.

```sh
kargo remove <group:artifact or key>... [--dev] [-t <target>]
```

| Option | Meaning |
|---|---|
| `--dev` | Remove from `[dev-dependencies]` |
| `-t, --target <target>` | Remove from `[target.<target>.dependencies]` |
| `-p, --package <name>` | The workspace member whose `kargo.toml` to edit |

Path dependencies are removed by their key: `kargo remove mathlib`. Without an option, the dependency is removed from `[dependencies]`.

## `kargo outdated`

List the dependencies in `kargo.toml` that have newer versions in your repositories.

```sh
kargo outdated [group:artifact or key]... [--pre]
```

```console
$ kargo outdated
name                                    current  compatible  latest   kind
org.jetbrains.kotlinx:kotlinx-datetime  0.6.0    0.6.2       0.8.0    dependencies
io.mockk:mockk                          1.13.0   1.14.11     1.14.11  dev-dependencies
```

| Column | Meaning |
|---|---|
| `current` | The version in `kargo.toml` |
| `compatible` | The newest version with the same major version (for `0.x`, the same minor version), or `-` if there is none |
| `latest` | The newest version, or `-` if `current` is the newest |
| `kind` | The table the dependency is declared in |

| Option | Meaning |
|---|---|
| `names` | Only check these dependencies |
| `--pre` | Consider pre-release versions. Dependencies already on a pre-release always consider them |
| `-p, --package <name>` | The workspace member to check |

A dependency stays on its variant: `33.0.0-jre` is compared with other `-jre` versions, and a plain version never moves to a `-compat` one. Dependencies declared with a version range or without a version (managed by a BOM) are skipped. The command only reads; nothing is changed.

## `kargo upgrade`

Raise the versions in `kargo.toml` to the newest compatible versions, keeping the file's formatting and comments.

```sh
kargo upgrade [group:artifact or key]... [options]
```

```console
$ kargo upgrade
note: `org.jetbrains.kotlinx:kotlinx-datetime` 0.8.0 is available but not compatible; use `--incompatible` to upgrade to it
   Upgrading org.jetbrains.kotlinx:kotlinx-datetime v0.6.0 -> v0.6.2 in [dependencies]
   Upgrading io.mockk:mockk v1.13.0 -> v1.14.11 in [dev-dependencies]
```

| Option | Meaning |
|---|---|
| `names` | Only upgrade these dependencies |
| `-i, --incompatible` | Upgrade to the newest version even across a major version (or a `0.x` minor version) |
| `--pre` | Consider pre-release versions |
| `--dry-run` | Print the changes without writing `kargo.toml` |
| `-p, --package <name>` | The workspace member whose `kargo.toml` to edit |

Dependencies inherited with `workspace = true` are skipped; upgrade them in the workspace's `kargo.toml`. "Compatible" follows the same rule as the `compatible` column of [`kargo outdated`](#kargo-outdated). `kargo.lock` is re-resolved on the next build because the manifest changed.

## `kargo target`

Enable, disable or list the targets of an existing project. `kargo target add` writes the same `[target.<name>]` table `kargo new` would, keeping the formatting and comments of `kargo.toml`, and scaffolds the files the target needs.

```sh
kargo target add <target>... [--lib | --app]
kargo target remove <target>...          # alias: rm
kargo target list [--format json]        # alias: ls
```

```console
$ kargo target add wasm
      Adding jvm target
note: kargo.toml had no [target] section, which means jvm only; [target.jvm] keeps the jvm target enabled
      Adding wasm target
note: created src-wasm/.gitkeep
note: run `kargo build --target wasm` to build it
```

| Option | Meaning |
|---|---|
| `<target>...` | `jvm`, `wasm`, `native` or `android`; space- or comma-separated |
| `--lib` | `add`: a Native or Android library (`kind = "library"`) |
| `--app` | `add`: a Native executable or an Android application |
| `--format json` | `list`: print JSON (`enabled` with each target's settings, and `available`) |
| `-p, --package <name>` | The workspace member whose `kargo.toml` to edit or list |

`kargo target add`:

- Writes the defaults of `kargo new`: `environment = "js"` for Wasm, `platforms = ["host"]` and a `kind` for Native, and `namespace`, `kind` and `min-sdk = 24` for Android (the namespace is the package's `group` plus its name).
- Adds `[target.jvm]` first when `kargo.toml` has no `[target]` section, because such a project is implicitly JVM and would stop building for the JVM once any other `[target.*]` table exists.
- Without `--lib` or `--app`, picks an application when the sources declare a `main` function (or the manifest sets `main-class` or a `[[bin]]`), and a library otherwise.
- Creates only missing files and never overwrites one: `src-<target>/.gitkeep` when `src-<target>/` does not exist, and for Android `android/AndroidManifest.xml` plus, for an application, a launcher activity in `src-android/MainActivity.kt`.
- Is idempotent: a target that is already enabled prints a note and leaves `kargo.toml` alone.
- Warns when an enabled framework does not support the new target (Spring Boot on Wasm, for example).

`kargo target remove` deletes `[target.<name>]` with its `dependencies` and `cinterop` sub-tables. It keeps `src-<target>/`, `tests-<target>/` (and `android/`) on disk and lists them. Removing the last enabled target is an error: add another target first.

## `kargo tree`

Show the resolved dependency graph. Conflicts are shown as `requested -> selected`; repeated subtrees are marked `(*)`.

| Option | Default | Meaning |
|---|---|---|
| `-t, --target <target>` | first enabled | Which target |
| `-c, --configuration <c>` | `runtime` | `compile`, `runtime` or `test` |
| `--depth <n>` | unlimited | Maximum depth |
| `-d, --duplicates` | | Only modules requested in several versions |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | current package | Which packages to show |
| `--format <fmt>` | `text` | `text` or `json`. `json` prints the whole resolved graph, with each module's version, files and direct dependencies; it can't be combined with `--depth` or `--duplicates`. When several packages are selected, the JSON output is an array with one document per package |

Path dependencies are listed first, as `mathlib v0.1.0 (<directory>)`, each with the path dependencies it uses in turn (the module graph); their Maven dependencies appear in the graph next to yours. With `-c compile`, only the path dependencies there are to compile against are listed: a path dependency's own path dependencies are only there at runtime unless they are `exported` ([what comes along](../../guides/workspaces/#what-comes-along)).

## `kargo classpath`

Print the resolved classpath (JVM) or klib path (Wasm, Native) of a target: the jars on disk, where `kargo tree` shows coordinates. Use it to point any tool, script or editor at a project's libraries.

```console
$ kargo classpath --format lines
/home/me/.kargo/cache/maven/io/ktor/ktor-server-core-jvm/3.2.3/ktor-server-core-jvm-3.2.3.jar
/home/me/.kargo/cache/maven/org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21.jar
...
```

The entries are the ones the build compiles with, in the same order: path dependencies first (built if needed), then the resolved Maven artifacts, including the Kotlin stdlib and what [frameworks](../../guides/frameworks/) add. The package's own classes are not included. Resolution follows `kargo.lock`, `--locked` and `--offline`, and may download dependencies and update `kargo.lock`, as `kargo tree` does.

| Option | Default | Meaning |
|---|---|---|
| `-t, --target <target>` | `jvm` | Which target. Without it, the JVM target, or the package's only Wasm or Native target. A Native target prints the klibs of its first platform; Android's classpath comes from Gradle (`kargo android -- dependencies`) |
| `-s, --scope <scope>` | `compile` | `compile` (what the main code compiles against), `runtime` (what it runs with) or `test` (what the tests compile and run against, dev-dependencies included) |
| `--format <fmt>` | `path` | `path`: one line joined with the platform's path separator (`:`, or `;` on Windows), ready for `-cp`. `lines`: one entry per line. `json`: an object with `package`, `target`, `platform`, `scope` and `classpath` |
| `-p, --package <name>` | current package | Which package |

## `kargo package`

Build a self-contained, runnable fat jar with all runtime dependencies, path dependencies included: `target/<profile>/jvm/<name>-<version>-all.jar` (`target/<profile>/<name>/jvm/` in a workspace). The path of the jar is printed on stdout.

`--format` ships a JVM application another way. Each prints the path of what it built on stdout:

| Format | Builds |
|---|---|
| `jar` | The fat jar above (the default) |
| `jlink` | `target/<profile>/jvm/package/<name>-<version>/`: the jars in `lib/`, a Java runtime with only the JDK modules the program uses in `runtime/`, and `bin/<name>` and `bin/<name>.bat` launchers. Runs on machines without Java |
| `native-image` | `target/<profile>/jvm/package/<name>` (`<name>.exe` on Windows): a native executable built by GraalVM's `native-image` |
| `installer` | An installer built by `jpackage` into `target/<profile>/jvm/package/`: `.msi` on Windows, `.dmg` on macOS, `.deb` on Linux, or the [`[package.installer]`](../manifest/#packageinstaller) `type` |

`jlink`, `native-image` and `installer` need a main class and build for the OS and CPU Kargo runs on. An installer can also register file associations, and on macOS be signed and notarized. See [Distributing applications](../../guides/distributing/).

A library (a project without a `main` function or `main-class`) gets what you would publish instead: `target/<profile>/package/<name>-<version>.jar` with only its own classes (no Kotlin standard library, no `Class-Path`) and `<name>-<version>-sources.jar` next to it. Pass `--fat` for the fat jar anyway. `kargo publish --dry-run` shows everything a [publication](../../guides/publishing/) holds, POM included.

With `-t android` (the default when the JVM target is off and Android is on), it builds the Android App Bundle (`.aab`) and APK of an application, or the AAR of a library, for every [product flavor](../../guides/android/#product-flavors), and copies them to `target/<profile>/android/`. Their paths are printed on stdout, App Bundles first. They are signed when [`[target.android.signing]`](../manifest/#targetandroid) has an entry for the build type; an unsigned release build gets a warning. See [Release builds](../../guides/android/#release-builds).

| Option | Meaning |
|---|---|
| `-r, --release` | Use the release profile |
| `--profile <name>` | Use a custom profile from kargo.toml, such as `staging`, instead of `dev` (see [Profiles](../manifest/#profilename)) |
| `-t, --target <target>` | `jvm` (the default when enabled) or `android` |
| `-p, --package <name>` | The package to build the jar for; required at the root of a workspace with several members |
| `--bin <name>` | Launch this [`[[bin]]`](../manifest/#bin) instead of the main class; the jar is `<bin>-<version>-all.jar` (JVM) |
| `--fat` | Build the self-contained `<name>-<version>-all.jar` even for a library (JVM) |
| `--format <format>` | `jar` (default), `jlink`, `native-image` or `installer` (JVM) |
| `--desktop` | Package the [Compose Desktop](../../guides/compose/#distributing) application: an installer for this OS unless `--format` says otherwise, with the JDK modules, options and Skiko native library Compose needs. Needs `[frameworks] compose` |
| `--platforms <list>` | With `--desktop --format jar`: the platforms whose Skiko natives the jar carries instead of only the host's, so it runs on each of them. `all`, or some of `windows-x64`, `windows-arm64`, `macos-x64`, `macos-arm64`, `linux-x64`, `linux-arm64` |

## `kargo publish`

Build a library with the release profile and publish it to a Maven repository: the local one (`~/.m2/repository`), a [`[repositories]`](../manifest/#repositories) entry, or Maven Central. See [Publishing a library](../../guides/publishing/).

```sh
kargo publish [--repository <name> | --local] [--dry-run]
```

Every target except Android is published: a JVM-only library as one Maven module (`<name>.jar`), anything else as a multiplatform library with a root module and one module per platform (`<name>-jvm`, `<name>-wasm-js`, `<name>-linuxx64`, ...). Each module gets a POM, Gradle module metadata, a sources jar and a javadoc jar, the checksums `.md5`, `.sha1`, `.sha256` and `.sha512`, and `.asc` signatures when [signing](../../guides/publishing/#signing) is on. Everything is staged in `target/release/publish/repository` first.

| Option | Meaning |
|---|---|
| `--repository <name>` | Where to publish: a `[repositories]` entry, `maven-central` or `maven-local`. Defaults to `repository` in [`[publish]`](../manifest/#publish) |
| `--local` | Publish to the local Maven repository, like `--repository maven-local` |
| `--dry-run` | Build, stage and sign everything, but upload nothing |
| `-p, --package <name>`, `--workspace` | Which packages to publish. Path dependencies are published before the packages that use them; members with `publish = false` are skipped when the whole workspace is published |

The package needs a `group` in `[package]`. Maven Central also needs `description`, `license`, `authors` and `repository`, a release (non-SNAPSHOT) version, and signatures; Kargo checks them before building.

A package with a main class also gets a `kargo.main-class` property in the POM of its JVM module, which `kargo install group:artifact` reads to know what to run.

## `kargo install`

Build or download a Kotlin command-line tool and put it on your `PATH`. See [Installing command-line tools](../../guides/installing-tools/).

```sh
kargo install --path <dir>
kargo install --git <url> [--tag <tag> | --branch <branch> | --rev <sha>]
kargo install <group:artifact[:version]> [--repository <url>]...
kargo install --force <name>
kargo install --list
```

| Option | Meaning |
|---|---|
| `--path <dir>` | Install the project in `<dir>` (`.` for the current directory) |
| `--git <url>` | Install the project at the root of a git repository. `--tag`, `--branch` and `--rev` pick what to check out; without them, the default branch |
| `<group:artifact[:version]>` | Install an application published to Maven: its jars and runtime dependencies, run by its main class. The newest release when there is no version |
| `<name>` | With `--force`: install an installed program again from where it came from |
| `--repository <url>` | With a Maven coordinate: a repository to look in before Maven Central (repeatable) |
| `--bin <name>` | Install only this program (repeatable). By default every entry point is installed: the project's own and each `[[bin]]` |
| `-t, --target <target>` | `jvm` (the default when enabled) or `native` |
| `--format <fmt>` | `jar` (default) runs the fat jar with the JDK of the build; `jlink` installs a trimmed Java runtime with the program, so no Java is needed |
| `-f, --force` | Replace a program that is installed already, or a launcher in `bin/` that Kargo did not write |
| `--list` | List the installed programs with their version, source and launchers |
| `--root <dir>` | Install in `<dir>/bin` and `<dir>/install` instead of the Kargo home. Also read from [`KARGO_INSTALL_ROOT`](../environment/) |
| `-p, --package <name>` | The workspace member to install |

Projects are built with the `release` profile and honor `--locked`, `--offline` and `--refresh`. Launchers go to `~/.kargo/bin`, the program's files to `~/.kargo/install/<name>/<version>/`, and `~/.kargo/install/.installed.toml` records them. A JVM launcher runs the jar with the JDK the build used, by its path; a Native program is copied to `bin/` as it is.

## `kargo uninstall`

Remove programs installed with `kargo install`: their launchers, files and entries in `.installed.toml`. Nothing is removed if one of the names is not installed.

```sh
kargo uninstall <name>... [--root <dir>]
```

## `kargo fetch`

Download all dependencies and toolchains so later builds can run offline. For each enabled target this is the Kotlin compiler, its plugins and the dependencies. The JVM and Wasm targets also get the JDK that `[toolchain] jdk` asks for (`system`, the default, is the JDK on the machine and is left alone). The JVM target gets the JUnit console launcher that `kargo test` runs and, in a package with `benches/`, the libraries and the harness generator of [`kargo bench`](#kargo-bench) (locked as `jvm/bench` and `jvm/bench-tools`), and the Wasm target gets the [npm packages](../../guides/wasm/#npm-packages) of its Maven dependencies when npm, pnpm or bun is on `PATH`; without one, Kargo warns and skips them. For Android this is the [Gradle distribution](../../guides/android/#gradle) and its wrapper; Gradle fetches Android dependencies itself during the build.

| Option | Meaning |
|---|---|
| `-t, --target <target>` | Only this target (repeatable) |
| `--sources` | Also download the `-sources.jar` of every resolved library (JVM and klib targets), so editors can show and step into library code. Libraries that publish none are skipped. Sources jars are not recorded in `kargo.lock` |
| `--tools` | Also download the tools of [`kargo doc`](../../guides/documentation/) (Dokka), [`kargo fmt`](../../guides/formatting-and-linting/) (ktlint) and [`kargo lint`](../../guides/formatting-and-linting/) (detekt), so they work with `--offline` |
| `-p, --package <name>`, `--workspace` | Which packages to fetch for |

```console
$ kargo fetch --sources
 Downloading kotlin-stdlib-2.4.21-sources.jar
     Fetched 12 sources jars
```

## `kargo vendor`

Copy every Maven file a build of the project needs into a directory laid out as a Maven repository (`group/artifact/version/...`, with POMs, `.module` files, `maven-metadata.xml` and `.sha1` checksums), so a build with `--vendored` resolves from it alone, for example on a machine without network access. It copies what [`kargo fetch`](#kargo-fetch) downloads for the enabled targets, and every file of `kargo.lock`, checked against its SHA-256. The Android target is skipped, since Gradle resolves its dependencies. See [Offline and corporate builds](../../guides/offline-and-mirrors/).

```sh
kargo vendor [path] [--toolchains] [--sync]
```

| Option | Meaning |
|---|---|
| `path` | The directory to write, relative to the project root (default: `[source] directory` of `.kargo/config.toml`, else `vendor`) |
| `--toolchains` | Also copy the Kotlin compiler and the Kotlin/Native distribution for this host |
| `--sync` | Remove the files of the directory that the project no longer needs |

```console
$ kargo vendor
    Vendored 214 files into /home/me/app/vendor (214 copied)
note: to build from it, pass `--vendored`, or add this to .kargo/config.toml:

[source]
vendored = true
```

## `kargo update`

Re-resolve dependencies and rewrite `kargo.lock`. Cached repository metadata is ignored, so new versions within version ranges and new builds of [SNAPSHOT versions](../../guides/dependencies/#snapshot-versions) are picked up.

```sh
kargo update [group:artifact]... [--precise <version>] [--dry-run]
```

| Option | Meaning |
|---|---|
| `modules` | Only update these locked modules (direct or transitive). Every other module keeps its locked version (a SNAPSHOT, its locked build) |
| `--precise <version>` | Lock the one given module at exactly this version, for example to pick up a security fix in a transitive dependency or to step back from a broken release |
| `--dry-run` | Print the changes without writing `kargo.lock` |

Each change is printed as `Updating`, `Adding` or `Removing`:

```console
$ kargo update org.jetbrains.kotlinx:kotlinx-io-core --precise 0.7.0
    Updating org.jetbrains.kotlinx:kotlinx-io-core v0.6.0 -> v0.7.0
     Updated kargo.lock
```

In a workspace, or with path dependencies, `kargo update` re-resolves every package, since they share one `kargo.lock`. `kargo update` changes only `kargo.lock`, never `kargo.toml`; use [`kargo upgrade`](#kargo-upgrade) to change the versions you declared. A `--precise` version stays locked until `kargo.toml` changes or you run `kargo update` for that module again. See [kargo.lock](../lockfile/#commands).

## `kargo clean`

Delete everything in `target/` except the build-directory lock, `target/.kargo-lock`. In a workspace, that is the workspace's `target/`.

| Option | Meaning |
|---|---|
| `-r, --release` | Only delete the release profile's output |
| `--profile <name>` | Only delete the output of a custom profile (`target/<name>/`), even one that kargo.toml no longer defines |

## `kargo env`

Show the environment `kargo run` and `kargo test` start programs with: each variable, the source that set it (`shell`, `[env]`, `[profile.<name>.env]` or a `.env` file with its line number) and its value.

```sh
kargo env [--release | --profile <name>] [--show-values] [--check] [--env-file <path>] [--no-env-file] [-p <name>]
```

| Option | Meaning |
|---|---|
| `-r, --release` | Show the release profile's variables |
| `--profile <name>` | Show a custom profile's variables, which include those it inherits |
| `--show-values` | Print the values. Without it they show as `********`, because they often hold secrets |
| `--check` | Fail (exit code 101) listing each required variable that is not set, and each key of `.env.example` that neither the shell nor a source sets. For CI |
| `--env-file <path>`, `--no-env-file` | As for `kargo run` |
| `-p, --package <name>` | The package, at the root of a workspace with several members |

A required variable that nothing sets is listed as `missing (required)`; the listing itself succeeds, and `--check` makes it a failure. See [Environment variables](../../guides/environment/#check-the-result-with-kargo-env).

## `kargo task`

List tasks, or run one. Extra arguments are appended to the task's last command.

```sh
kargo task               # list
kargo task --list        # the same
kargo task [--env-file <path>] [--no-env-file] <name> [args]
kargo <name> [args]      # shorthand
```

A package task starts with the [project environment](../../guides/environment/): `--env-file <path>` (repeatable) and `--no-env-file` choose its `.env` files, as they do for `kargo run`. They can come before the name or right after it, where they are taken from the front of the arguments: `kargo deploy --env-file .env.staging --dry-run` passes only `--dry-run` to the task. Workspace tasks run at the root and get no project environment.

In a workspace, a member's directory runs that member's tasks, and the root runs a `[workspace.tasks]` task; a name that only members define runs in every member that has it, in dependency order. See [Workspace tasks](../../guides/workspaces/#tasks-and-plugins).

Inside a project, `kargo --help` also lists its tasks. A task named like a built-in command, such as `build` or `test`, can only run as `kargo task <name>`; the listing marks it and the commands that work on a package [warn about it](../warnings/#tasks-hidden-by-built-in-commands).

## `kargo plugins`

List hook plugins, compiler plugins and `kargo-*` subcommands.

## `kargo android`

Run Gradle in the generated Android project (`target/android`).

```sh
kargo android <gradle args...>
kargo android lint
kargo android connectedAndroidTest
```

## `kargo toolchain`

Manage JDKs, GraalVMs, Kotlin/Native and Gradle distributions, and the Kotlin language servers [`kargo lsp`](#kargo-lsp) runs.

| Subcommand | Meaning |
|---|---|
| `list` (`ls`) | Installed toolchains, disk usage and what this project uses |
| `install [jdk\|graalvm\|native\|gradle\|kotlin-lsp\|kls\|kotlin-debug-adapter] [version]` | Install a toolchain; with no arguments, whatever the project needs. `graalvm` takes `25` (GraalVM Community Edition) or `oracle-25` (Oracle GraalVM). `gradle` without a version installs the one the project uses; `kotlin-lsp` and `kls` without a version install the one `kargo lsp` uses, `kotlin-debug-adapter` the one `kargo dap` uses |
| `remove` (`rm`) `<jdk\|graalvm\|native\|gradle\|kotlin-lsp\|kls\|kotlin-debug-adapter> <version>` | Remove one. For JDKs and GraalVMs, a major version removes all its releases |
| `available` | JDK versions available for download (LTS marked) |
| `which` | The toolchains this project resolves to |

## `kargo self`

Update or remove Kargo itself. These commands never read a `kargo.toml`, so they work anywhere, even in a project whose [`[toolchain] kargo`](../manifest/#toolchain) asks for a newer Kargo than the one you have. See [Installation](../../getting-started/installation/).

```sh
kargo self update [--version <version>] [--check]
kargo self uninstall [--purge]
```

| Subcommand | Meaning |
|---|---|
| `update` | Download the latest release for this platform, check the archive's SHA-256 against the release's `SHA256SUMS`, and replace the running binary. Nothing is replaced when the checksum does not match. On Windows the running `kargo.exe` is renamed to `kargo.exe.old` first and deleted by the next `kargo` |
| `update --version <version>` | Install that version instead of the latest. An older one downgrades |
| `update --check` | Only say whether a newer release exists |
| `uninstall` | Remove the binary and the `PATH` entries the install scripts added (the marked block in shell startup files, or the entry in the user `PATH` on Windows). `~/.kargo` stays |
| `uninstall --purge` | Also delete `~/.kargo` (`KARGO_HOME`): toolchains, the Maven cache, installed tools. Stops the compiler daemons first |

`kargo self` refuses to change a Kargo that a package manager installed, and prints that manager's command instead: `brew upgrade kargo`, `scoop update kargo`, `winget upgrade kargo`, `sdk upgrade kargo`, `nix profile upgrade kargo`, `cargo install --locked --force --git ...` for a Kargo in `~/.cargo/bin` (or `$CARGO_HOME/bin`), and your distribution's package manager for `/usr/bin/kargo`. It decides by where the binary is. `update --check` always works.

Releases come from `https://github.com/enekocm7/kargo/releases`. `KARGO_RELEASES_URL` points Kargo at a mirror, or at a directory with `file://`, which must be laid out like the release downloads: `latest/download/VERSION`, `download/v<version>/SHA256SUMS` and `download/v<version>/kargo-<version>-<target>.tar.gz` (`.zip` on Windows). `--offline` makes `kargo self update` fail, except for a `file://` mirror.

**The update notice.** `kargo --version` looks the newest version up at most once a day (with a two-second timeout; the answer is remembered in `~/.kargo/update-check.json`) and, when it is newer than the running one, prints a note on stderr: ``note: update available: kargo 0.3.0 (this is kargo 0.2.0); run `kargo self update` ``, also at most once a day. Nothing is printed or fetched with `--offline`, with `-q`, when `KARGO_NO_UPDATE_CHECK` is set, in CI (`CI` is set) or when stderr is not a terminal. No other command checks.

## `kargo daemon`

Show or stop the background compiler daemons that keep the Kotlin compiler warm between builds. See [Build performance](../../guides/build-performance/#the-compiler-daemon).

| Subcommand | Meaning |
|---|---|
| `status` | List running daemons with their process ID and state directory |
| `stop` | Stop every running daemon; the next build starts a new one |

## `kargo info`

Print project information: package, targets, path dependencies, repositories and paths. A repository that a [mirror](../../guides/offline-and-mirrors/#repository-mirrors) or `--vendored` replaces shows where it is fetched from (`maven-central (via https://nexus.corp/central)`), and a `config:` line lists the [configuration files](../../guides/offline-and-mirrors/#configuration-files) Kargo read. At the root of a workspace, it prints the members, the default members, the Kotlin version, the lockfile, the target directory and the dependency graph between the members (`server -> core`); with `--format json`, each member lists its `dependencies`.

| Option | Default | Meaning |
|---|---|---|
| `--format <fmt>` | `text` | `text` or `json`. `json` also lists every dependency and repository, with the `effective-url` it is fetched from. See [Machine-readable output](../machine-readable-output/#kargo-info---format-json) |

## `kargo metadata`

Print a JSON description of the whole project for editors, language servers and other tools: every package with its source, test and resource roots, output directories, compiler arguments and plugins, entry points, tasks, and the resolved classpath (JVM) or klib path (Wasm, Native) of every target and scope. Resolving may download dependencies and update `kargo.lock`, as `kargo tree` does.

```sh
kargo metadata
kargo metadata --no-deps
kargo metadata --fingerprint
```

| Option | Meaning |
|---|---|
| `--format <fmt>` | `json`, the only format (default) |
| `--no-deps` | Leave out the resolved dependencies, plugin jars and the JDK. Nothing is downloaded or locked |
| `-r, --release` | Describe the release profile's output directories and compiler arguments |
| `--profile <name>` | Describe a custom profile's output directories and compiler arguments |
| `--fingerprint` | Only print a hash of every `kargo.toml` and `kargo.lock`. It is the `fingerprint` of the document, and it changes when the metadata may have changed |

The output has a `"version": 1` field and is described in [Machine-readable output](../machine-readable-output/#kargo-metadata). See [Editor integration](../../guides/editors/) for how editors use it.

## `kargo lsp`

Run the language server for editors, speaking LSP over stdin and stdout. Kargo answers everything about `kargo.toml` (diagnostics, completion, hover, Maven coordinates and versions, hints at newer versions, code actions that make the edits of `kargo add`, `kargo target add` and `kargo upgrade`) and the `kargo/runnables` request itself. Kotlin code goes to [Kargo's own Kotlin server](../../guides/kotlin-server/), built into `kargo` (completion, hover, go to definition, references, rename and code actions), or to a Kotlin language server Kargo downloads on first use and describes the project to.

```sh
kargo lsp                       # Kargo's own Kotlin server
kargo lsp --server kotlin-lsp   # JetBrains' Kotlin LSP behind Kargo's server
kargo lsp --server kls          # fwcd's kotlin-language-server instead
kargo lsp --server none         # kargo.toml support and runnables only
```

| Option | Default | Meaning |
|---|---|---|
| `--server <name>` | `kargo` | The Kotlin language server: `kargo` (built in), `kotlin-lsp` (JetBrains), `kls` (fwcd) or `none` |
| `--server-path <path>` | | Run this server executable instead of the one Kargo downloads. It is a Kotlin LSP unless `--server kls` says otherwise |
| `--klib-targets` | | Also describe Wasm and Native sources to the Kotlin LSP. The pinned Kotlin LSP can't read klibs yet, so their libraries don't resolve. See [Multiplatform projects](../../guides/editors/#multiplatform-projects) |

With check on save, saving a Kotlin or Java file runs `kargo check` in the background and adds its diagnostics for every target to the Kotlin server's; `checkTargets` limits the targets. It is on with Kargo's own server and needs `{ "checkOnSave": true }` in the editor's `initializationOptions` with the others. See [Check on save](../../guides/editors/#check-on-save).

Kargo's own Kotlin server downloads the sources jars of dependencies in the background, as `kargo fetch --sources` does, and loads them without a restart; `{ "fetchSources": false }` in `initializationOptions` turns that off. See [Libraries](../../guides/kotlin-server/#libraries).

Editors that pass `--stdio` may keep it; Kargo always uses stdio. The exit code is 0 after `shutdown` and `exit`, 1 if the editor exits without `shutdown`. See [Editor integration](../../guides/editors/#language-server) for editor setups, the files it writes, the [code actions](../../guides/editors/#code-actions-in-kargotoml) and the `kargo/runnables` request.

## `kargo ide`

Write the project description a Kotlin language server reads, for editors that run JetBrains' Kotlin LSP or fwcd's kotlin-language-server themselves instead of [`kargo lsp`](#kargo-lsp). The files are the ones `kargo lsp` writes.

```sh
kargo ide           # workspace.json, for the Kotlin LSP
kargo ide --kls     # kls-classpath (kls-classpath.bat on Windows), for kotlin-language-server
kargo ide --zed     # .zed/tasks.json, for Zed's run buttons
```

| Option | Meaning |
|---|---|
| `--kls` | Write `kls-classpath` instead of `workspace.json` |
| `--klib-targets` | Also describe Wasm and Native sources in `workspace.json`. See [Multiplatform projects](../../guides/editors/#multiplatform-projects) |
| `--zed` | Write `.zed/tasks.json` instead: tasks that run `kargo run` and `kargo test` from the run buttons of Zed's Kotlin extension, the project's programs, tests and tasks, and everyday commands. Needs no dependency resolution. See [Zed](../../guides/editors/#run-and-test-in-zed) |

Kargo keeps the file current: after `kargo build`, `check`, `test`, `fetch`, `add`, `remove`, `update`, `upgrade` and `target`, it writes it again if `kargo.toml` or `kargo.lock` changed, and prints `Wrote workspace.json` (or `Wrote .zed/tasks.json`). A file Kargo didn't write is left alone, with a warning. If the dependencies can't be resolved, the project is described without them and the command fails with the reason. See [Without `kargo lsp`](../../guides/editors/#without-kargo-lsp).

## `kargo dap`

Run the debug adapter for editors: the [Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/) over stdin and stdout, in the project of the current directory.

```sh
kargo dap
```

Kargo runs [fwcd's kotlin-debug-adapter](https://github.com/fwcd/kotlin-debug-adapter) on the project's JDK, downloading it on first use like a [toolchain](../../guides/toolchains/#kotlin-language-servers), and passes the editor's messages through. It adds what the adapter lacks for a Kargo project:

| Request | What Kargo does |
|---|---|
| `launch` | Runs `kargo <args>` with `--debug` (and `-t jvm` unless `args` names a target), sends what the program prints to the editor's debug console, and attaches once the JVM waits for the debugger. A program that exits first fails the request with its exit code |
| `attach` | Attaches to a JVM started with `kargo run --debug` or `kargo test --debug` |

| Launch argument | Default | Meaning |
|---|---|---|
| `args` | `["run"]` | The arguments of `kargo`, starting with `run` or `test`: `["run", "--bin", "server"]`, `["test", "com.example.MathTest.adds"]` |
| `cwd` | the project root | The directory to run `kargo` in, relative to the project root |
| `env` | | Environment variables for `kargo` and the program |

| Attach argument | Default | Meaning |
|---|---|---|
| `port` | `5005` | The port the JVM listens on |
| `hostName` | `127.0.0.1` | Its host |

Both take `timeout` (milliseconds to try connecting, default 10000) and `logLevel` (`INFO`, `DEBUG`, `TRACE`, ...), which kotlin-debug-adapter logs to the debug console. Disconnecting stops a launched program. `--adapter-path <path>` runs another debug adapter executable instead of the one Kargo downloads. See [Debugging](../../guides/editors/#debugging).

## `kargo completions`

Print a shell completion script for `bash`, `zsh`, `fish`, `powershell` or `elvish`. It completes commands, flags and targets, plus the current project's tasks, `kargo-*` plugins, framework names and installed toolchains. See [Shell completions](../../getting-started/installation/#4-shell-completions-optional) for how to load it.

```sh
kargo completions bash
```

## `kargo <name>`

If `<name>` is a task in `kargo.toml` (or, in a workspace, in `[workspace.tasks]` or in the members' `[tasks]`, as for [`kargo task`](#kargo-task)), run it. Otherwise run the executable `kargo-<name>` from `~/.kargo/bin` or `PATH`.
