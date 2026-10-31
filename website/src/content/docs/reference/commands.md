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

The commands the short help leaves out (`outdated`, `upgrade`, `audit`, `target`, `info`, `plugins`, `android`, `daemon`, `metadata`, `classpath`, `ide`, `lsp`, `dap`, `self` and `completions`) work as usual, and `kargo help <command>` shows each one's options. Shell completions offer them too.

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
| `--require-checksums` | Fail on a download whose repository publishes no `.sha256` or `.sha1` for it, instead of accepting it unverified (also `KARGO_REQUIRE_CHECKSUMS=1`). Files `kargo.lock` pins are checked against it instead. See [Requiring published checksums](../lockfile/#requiring-published-checksums) |
| `--system-gradle` | Run Android builds with the `gradle` on `PATH` instead of a Kargo-managed Gradle (also `KARGO_SYSTEM_GRADLE=1`). A project `gradlew` still wins |
| `--manifest-path <PATH>` | Use this `kargo.toml` instead of searching from the current directory |
| `--color <WHEN>` | When Kargo's own output uses colors: `auto` (the default: on a terminal, and off when `NO_COLOR` is set), `always` or `never`. The compilers' and your program's output is not affected |
| `-V`, `--version` | Print the version. With `-v` (`kargo -vV`) also the release and the target it was built for (`host: x86_64-unknown-linux-gnu`). When a newer release exists, a note on stderr says so, at most once a day (see [`kargo self`](#kargo-self)) |
| `-h`, `--help` | Print help |

**Exit codes:** `0` on success, `101` when a build or test fails or Kargo reports an error (as in Cargo). `kargo run` exits with your program's exit code (`128 + n` when a signal `n` killed it on Linux and macOS). See [Continuous integration](../../guides/ci/).

Targets are `jvm`, `wasm`, `js`, `native` and `android`.

**Errors that help:** a mistyped command, `-p` package, `--framework`, `--platforms` value, `kargo.toml` key or target, or a `kargo add` coordinate, ends with `help: did you mean ...?`. An error with an entry in the [FAQ](../faq/) adds `help: see <link>`. See [Errors with a `help:` line](../faq/#errors-with-a-help-line).

## Package selection

In a [workspace](../../guides/workspaces/), commands apply to the default members at the workspace root (every member, unless [`default-members`](../manifest/#workspace) says otherwise) and to the current member inside a member's directory. These options pick packages explicitly:

| Option | Commands | Meaning |
|---|---|---|
| `-p, --package <name>` | `build`, `check`, `test`, `doc`, `fmt`, `lint`, `tree`, `audit`, `fetch`, `publish` | Only this package (repeatable) |
| `--workspace` | the same | Every member of the workspace |
| `--exclude <name>` | the same | Leave this member out (repeatable), with `--workspace` or at the workspace root |
| `-p, --package <name>` | `run`, `package`, `install`, `add`, `remove`, `outdated`, `upgrade` | The one package to work on; required at the root of a workspace with several default members |

`-p` takes any package of the project, including a path dependency that is not a member. Outside a workspace there is one package, so you never need these options.

### Concurrent commands

Commands that build or resolve a project (`build`, `check`, `run`, `test`, `doc`, `fmt`, `lint`, `tree`, `classpath`, `fetch`, `metadata`, `ide`, `package`, `publish`, `install --path`/`--git`, `update`, `clean`, `android`, `toolchain which`, and `lsp` while it imports the project) take an exclusive lock on `target/.kargo-lock` first. That lock guards both `target/` and `kargo.lock`. If another Kargo process (a second terminal, an IDE or a file watcher) holds the lock, the command waits for it to finish:

```console
$ kargo build
    Blocking waiting for file lock on build directory, held by `kargo run` (process 1234)
   Compiling my-app v0.1.0 (jvm)
```

`kargo run` releases the lock once the build is done, so a long-running program doesn't block other commands. Downloads to the shared cache in `~/.kargo` don't need the lock: each file is written to a temporary name and renamed into place, so two projects can fetch the same artifact at the same time.

The process that holds the lock writes its command and process id to `target/.kargo-lock.holder` and removes the file when it releases the lock, so the waiting command and [`kargo doctor`](#kargo-doctor) can name it. A workspace has one `target/`, so one process holds the lock for all its members.

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
| `-w, --with <list>` | | Frameworks: `compose`, `serialization`, `coroutines`, `ktor`, `lombok`, `spring-boot`, `benchmark`; or the presets `compose-multiplatform`: Compose on the jvm and wasm (browser) targets, besides the `--target`s you give, with a shared `App()` in `src/App.kt` (see [Compose](../../guides/compose/#one-ui-for-every-target)), and `compose-html`: a Compose HTML app on the js (browser) target instead of the default jvm one (see [Compose HTML](../../guides/compose/#compose-html-dom-elements)) |
| `--bench` | | Add the `benchmark` framework and a sample benchmark in `benches/SampleBench.kt`, as `--with benchmark` does. See [Benchmarking](../../guides/benchmarking/) |
| `--group <group>` | `com.example` | Maven group, also the base Kotlin package |
| `--name <name>` | directory name | Package name: starts with a letter, then letters, digits, `-` and `_`. A folder name such as `01-cli-app` that breaks this needs `--name` (for example `--name cli-app`) |
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

The generated code lives in the package `<group>.<name>`, with the group and the name lowercased and every character other than a letter or digit dropped (`my-app` and `my_app` both become `com.example.myapp`), so that ktlint's `package-name` rule and detekt's `PackageNaming` accept it. A segment that is a Kotlin or Java keyword gets a `_` suffix, so `kargo new class` uses `com.example.class_`.

Kargo runs `git init` in the new directory when `git` is on the `PATH`, unless you pass `--no-vcs` or the directory is already inside a git repository (a monorepo or an `examples/` folder), in which case the project joins that repository instead of nesting a new one. A `.gitignore` is written either way, unless one already exists.

:::tip
In a folder that is not a git repository, every `kargo new` creates its own repository. To generate a set of throwaway or test projects side by side, pass `--no-vcs` (`kargo new a --no-vcs`, `kargo new b --no-vcs`), or run `git init` once in the parent folder first so the projects join it.
:::

## `kargo init`

Same as `kargo new`, but in an existing directory (the current one by default). Existing files are not overwritten. A directory that already has a `kargo.toml` is refused; use [`kargo target add`](#kargo-target) to enable more targets in it.

```sh
kargo init [path] [options]
```

## `kargo import`

Convert a Maven, Gradle or Kotlin Toolchain project into a Kargo project: write the `kargo.toml` (a [workspace](../../guides/workspaces/) for a multi-module build), move the sources into [Kargo's layout](../../getting-started/project-layout/) with `git mv` inside a git repository, print a report and build the result. The old build files stay.

```sh
kargo import [path] [options]
```

| Option | Default | Meaning |
|---|---|---|
| `--dry-run` | | Print every `kargo.toml` and file move on stdout, and change nothing |
| `--no-build` | | Don't run `kargo build` when the import is done |
| `--from <maven\|gradle\|kotlin>` | the build tool whose files are there | Which build tool to read when the directory has the files of several; `amper` is an alias of `kotlin` |

```sh
kargo import --dry-run
kargo import
kargo import ../legacy-service --no-build
```

The command refuses a directory that already has a `kargo.toml`, plans every move before making one, and fails without changing anything when two files would land on the same path. A Maven project is translated exactly. A Gradle project and a Kotlin Toolchain project are read without running their build, so what the scripts or templates compute in code is listed in the report instead. If the final build fails, the project is imported anyway and the command exits with code 101. See [Importing a project](../../guides/importing/).

## `kargo init-ci`

Write a CI workflow for the project, from its `kargo.toml`.

```sh
kargo init-ci [--system github] [--junit] [--force]
```

```console
$ kargo init-ci
     Created .github/workflows/ci.yml
        note: left out actions/setup-node: no Wasm or JS target
        note: left out OS matrix: no Kotlin/Native target (the job runs on ubuntu-latest)
```

| Flag | Meaning |
|---|---|
| `--system <name>` | The CI system. Only `github` (GitHub Actions) for now, the default |
| `--junit` | Run `kargo test --report junit` and publish the reports in the run summary |
| `--force` | Replace the workflow if it exists. Without it, an existing file is an error |

It prints what it wrote and each step it left out, with the reason. See [Continuous integration](../../guides/ci/#generating-a-workflow) for the workflow and the `setup-kargo` action it uses.

## `kargo build` (`b`)

Compile all enabled targets, or the ones you name. Targets build at the same time; if one fails, the command reports that failure and exits with code 101.

| Option | Meaning |
|---|---|
| `--keep-going` | Build every target even if one fails, then report each failure and which targets failed and succeeded |
| `--xcframework` | Also combine the Apple frameworks of `kind = "framework"` into `target/<profile>/native/<name>.xcframework` (macOS with Xcode). See [C interop](../../guides/c-interop/#calling-kotlin-from-swift-and-objective-c) |
| `--swift-package`, `--cocoapods` | With `--xcframework`: also write `Package.swift` (a Swift package) or `<name>.podspec` (a CocoaPods spec) next to the XCFramework, for Xcode projects that use Swift Package Manager or CocoaPods. See [Swift Package Manager and CocoaPods](../../guides/c-interop/#swift-package-manager-and-cocoapods) |
| `--message-format <fmt>` | `human` (default) or `json`: print each compiler diagnostic as a JSON object on stdout. See [Machine-readable output](../machine-readable-output/#compiler-diagnostics) |
| `-r, --release` | Use the release profile |
| `--profile <name>` | Use a custom profile from kargo.toml, such as `staging`, instead of `dev` (see [Profiles](../manifest/#profilename)) |
| `-t, --target <target>` | Only this target (repeatable). Default: `[build] default-targets` (`[workspace.build]` in a workspace), else every enabled target |
| `--target-set <set>` | Instead of `-t`: `host` for the targets that build and run on this machine (JVM, Wasm, JS, and Native when this host's platform is in `platforms`), or `device` for the targets that run on a phone or an emulator (Android). An error when no enabled target is in the set. Ignores `[build] default-targets`. See [Target sets](../../guides/multiplatform/#target-sets) |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | Which packages to build ([package selection](#package-selection)) |
| `-j, --jobs <n>` | How many workspace members, and targets of each, build at the same time: a number, or `auto` (also `0`) for the CPUs within the free memory. Default: `KARGO_JOBS`, then `[build] jobs`, then `auto`. `-j 1` builds one thing at a time, in a fixed order. See [parallelism](../../guides/build-performance/#limiting-parallelism) |
| `--size-report` | Print Binaryen per-function metrics for a Wasm release build |

After a JVM build, `kargo build` prints a `Resolved` line for each enabled [framework](../../guides/frameworks/) with its version, and for Spring Boot the versions of the BOM's Spring Framework, Spring Security and Jackson modules on the classpath, so a version that changes the code you write (Jackson 2 or 3, Spring Boot 3 or 4) is visible without opening `kargo.lock`. [`kargo fetch`](#kargo-fetch) prints the same lines.

[Path dependencies](../../guides/workspaces/) are built first, for the same targets. In a workspace, members build in dependency order, and independent members at the same time ([the build graph](../../guides/workspaces/#the-build-graph)); after a failure no new member starts (unless you pass `--keep-going`), and the error names the member and the target, such as ``could not build `core` (jvm)``.

Before compiling, `kargo build` (like `check`, `run`, `test` and `package`) runs the [`[[generate]]` steps](../../guides/code-generation/) that are out of date, and [warns](../warnings/) about projects that are valid but probably not what you meant: a target with no sources, Kotlin scripts (`.kts`) that are never compiled, a [task](../../guides/tasks-and-plugins/) hidden by a built-in command, and an odd package version.

For Wasm, a release build removes dead code, downloads pinned Binaryen on demand, and reports raw, gzip and Brotli sizes. Optional size budgets can fail the build, and `--size-report` asks Binaryen for per-function metrics. See [WebAssembly](../../guides/wasm/). A `kind = "library"` project also gets a `package.json` and TypeScript declarations.

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

In a workspace, `--keep-going` also continues with the other members after one fails. Every failure is printed, members that depend on a failed member are skipped with a note, and a summary ends the run with exit code 101:

```console
$ kargo build --workspace --keep-going
note: skipped `app`: a package it depends on failed
error: could not build 2 of 5 packages (core, cli); 1 skipped; 2 succeeded
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
| `--target-set <set>` | Instead of `-t`: `host` for the targets that build and run on this machine (JVM, Wasm, JS, and Native when this host's platform is in `platforms`), or `device` for the targets that run on a phone or an emulator (Android). An error when no enabled target is in the set. Ignores `[build] default-targets`. See [Target sets](../../guides/multiplatform/#target-sets) |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | Which packages to check |
| `-j, --jobs <n>` | How many workspace members, and targets of each, are checked at the same time, as for `kargo build` |

```sh
kargo check
kargo check --tests -t native
```

What each target skips:

| Target | `kargo check` does | Output |
|---|---|---|
| JVM | Compiles Kotlin and Java, but writes no jar and no `lib/`. Kotlin has no analysis-only mode on the JVM, so the classes go to the same place as `kargo build`'s, and a later build reuses them. | `target/<profile>/jvm/classes` |
| Wasm | Produces only the klib; skips linking the `.mjs`/`.wasm` module | `target/<profile>/check/wasm` |
| JS | Produces only the klib; skips linking the JavaScript module | `target/<profile>/check/js` |
| Native | Produces a klib for each platform; skips code generation and linking | `target/<profile>/check/native/<platform>` |
| Android | Runs Gradle's `compileDebugSources` (`compileReleaseSources` with `--release`), plus `compileDebugUnitTestSources` with `--tests`. Skips dexing and APK/AAR packaging | `target/android/build` |

`pre-build` [hooks](../../guides/tasks-and-plugins/#hooks) run first, so generated sources are checked too. `post-build` hooks don't run. `[kotlin] warnings-as-errors`, `--locked` and `--offline` behave as they do for `kargo build`, and a failed check exits with code 101.

## `kargo run` (`r`)

Build and run the program, or a single Kotlin file. Arguments after `--` go to the program.

```sh
kargo run -- --verbose               # the project's program
kargo run tools/migrate.kt up        # one .kt file with a main, compiled against the project
kargo run fetch.main.kts https://example.com   # a script, as `kargo script` runs it
kargo run -e 'println(1 + 1)'
```

| Option | Meaning |
|---|---|
| `<file>` | Run this `.kt` or `.kts` file instead of the project's program; the words after it go to the program. See [Running a single file](#running-a-single-file) |
| `-e, --expr <code>` | Run this Kotlin code as `kargo script -e` does. Every other word is then an argument |
| `-r, --release` | Use the release profile |
| `--profile <name>` | Use a custom profile from kargo.toml, such as `staging`, instead of `dev` (see [Profiles](../manifest/#profilename)) |
| `-t, --target <target>` | Target to run. Default: the first enabled of `jvm`, `native`, `wasm`, `js`, `android` |
| `-p, --package <name>` | The package to run; required at the root of a workspace with several members |
| `--main <name>` | Run another `main`: a JVM class such as `com.example.tools.MigrateKt`, or on Native an entry function such as `com.example.tools.main`. `--entry-point` is an alias |
| `--bin <name>` | Run the [`[[bin]]`](../manifest/#bin) with this name |
| `--example <name>` | Compile and run `examples/<name>.kt` against the project and its dev-dependencies |
| `--port <port>` | Wasm and JS browser apps: the port to serve on (default: `port` of `[target.wasm]` / `[target.js]`, else `8080`; `0` picks a free one) |
| `--open` | Wasm and JS browser apps: open the served page in your browser |
| `--debug[=<port>]` | JVM: start the program suspended until a debugger attaches over JDWP to `127.0.0.1:<port>` (default `5005`; `0` lets the JVM pick and print one). See [Debugging](../../guides/editors/#debugging) |
| `--native-agent` | JVM: run on GraalVM (`[toolchain] graalvm`) with its tracing agent, which records the reflection, resources and proxies the program uses as `native-image` configuration in `native-image/`. Later runs add to it. See [Native executables](../../guides/distributing/#native-executables) |
| `--env-file <path>` | Read this `.env` file after the standard ones, so its values win. Repeatable. Conflicts with `--no-env-file`. See [Environment variables](../../guides/environment/#choose-the-files-for-one-command) |
| `--no-env-file` | Read none of the `.env` files; the variables in kargo.toml and the shell still apply |
| `--flavor <name>` | Android: install this [product flavor](../../guides/android/#product-flavors) (repeatable, one per dimension; default: the first flavor of each dimension). Needs `-t android` |
| `-s, --device <serial\|avd>` | Android: run on this device: a serial from `kargo android devices`, the name of a virtual device (started if it is not running) or the `host:port` of a wireless device. Also `KARGO_ANDROID_DEVICE`. Without it, one device is used, several are chosen from a list, and with none Kargo asks before it starts or creates an emulator; without a terminal it fails and lists them. See [Devices and emulators](../../guides/android/#devices-and-emulators) |
| `--no-logcat`, `--logcat` | Android: leave once the app started, or (the default) show its log until it exits or Ctrl+C stops it. The exit code is 1 when the app crashed |
| `--stop-emulator` | Android: shut down an emulator that this run started once the app is done |
| `--headless`, `--cold-boot`, `--wipe-data`, `--boot-timeout <seconds>` | Android: how an emulator that Kargo has to start boots: without a window, without its snapshot, with erased user data, and how long to wait (default 300) |
| `--message-format <fmt>` | `human` (default) or `json` for the compiler diagnostics, as for `kargo build`. The program's own output still goes to stdout |
| `--watch` | Rebuild and restart the program when its files change, until you press Ctrl+C. A failed build prints its errors and leaves the running program alone. Conflicts with `--debug`. A Wasm or JS browser app is served by one server for all rounds, and its open pages reload after each successful build. An Android app is installed again and its new log followed. See [Watch mode](../../guides/watch/) |
| `--hot-reload`, `--hot` | Like `--watch`, but a JVM program keeps running: the classes a build changed are swapped into it (`Reloaded 3 classes in 180 ms`) and a Compose Desktop UI recomposes. A change the JVM cannot apply in place restarts the program. Runs on the project's JDK, and a Compose app on JetBrains Runtime, which Kargo downloads. Needs the jvm target and the project's own program (not `--example`); conflicts with `--debug`. See [Hot reload](../../guides/watch/#hot-reload-a-jvm-program) and [Compose](../../guides/compose/#hot-reload) |
| `-j, --jobs <n>` | How many targets and Native platforms build at the same time, as for [`kargo build`](#kargo-build-b) |
| `--clear` | With `--watch`: clear the terminal before each run |
| `--ignore <glob>` | With `--watch`: leave out the files matching this gitignore-style pattern, on top of `.gitignore`. Repeatable |

`--clear` and `--ignore` need `--watch`.

`--main`, `--bin`, `--example` and a file exclude each other and work for the JVM and Native targets. Without them, `kargo run` starts `main-class` / `entry-point` or the single `main` in the sources; with several, it lists them and any bins. See [Several programs in one project](../../guides/multiple-programs/).

The `Running` line quotes arguments that contain spaces or shell characters, so you can copy it: ``Running `java com.example.MainKt add "buy milk"` `` shows that `buy milk` is one argument.

`--port` and `--open` only apply to a Wasm or JS browser app; `kargo run` rejects them for any other target. A Wasm or JS project with `kind = "library"` has no program, so `kargo run` rejects it too.

### Running a single file

`kargo run <file>` runs one Kotlin file, wherever it is:

- **A `.kt` file in a project** (a `kargo.toml` in its directory or above it, wherever you run `kargo run`) is compiled like an `--example`: against the project's classes, dependencies and dev-dependencies, with its toolchain, frameworks and profile. It needs a `main`. It is not part of the project's build: its classes go to `target/<profile>/jvm/files/<name>/`, and an unchanged file is not compiled again. `--release`, `--target` (`jvm` or `native`), `--debug`, `--env-file` and `-p` apply; `--watch` does not.
- **A `.kts` script**, **a `.kt` file with a `/* kargo ... */` front matter**, and **a `.kt` file outside any project** run as [`kargo script`](#kargo-script) runs them: with their own dependencies, Kotlin version and JDK, cached in `~/.kargo/cache/scripts`. The project's options, such as `--release` or `--debug`, are errors there.

The first word after `run` is the file only when it ends in `.kt` or `.kts` and the file exists. Any other word, and every word after `--`, is an argument for the program, so `kargo run deploy` passes `deploy` to the project's program. Use `--` before arguments that start with `-`: `kargo run tools/migrate.kt -- --dry-run`.

The exit codes are those of `kargo run`: `101` when the file does not compile, the program's own code otherwise.

How each target runs:

| Target | Runs |
|---|---|
| JVM | `java -cp ... <main-class>` with `jvm-args` |
| Wasm | `node target/<profile>/wasm/<name>.mjs`; the arguments reach `main(args)` (see [Program arguments](../../guides/wasm/#program-arguments)). With `environment = "browser"` it serves `target/<profile>/wasm/` on `127.0.0.1` until you press Ctrl+C (see [Browser apps](../../guides/wasm/#browser-apps)) |
| JS | `node target/<profile>/js/<name>.mjs` (`.js` for CommonJS and UMD), with `--enable-source-maps` in dev builds; the arguments reach `main(args)`. With `environment = "browser"` it serves `target/<profile>/js/` like a Wasm browser app (see [JavaScript](../../guides/js/#browser-apps)) |
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
| `--no-fail-fast` | Test every target even if one fails, then print a result per target. In a workspace, also test the other members when one cannot be compiled or started. Overrides [`[test] fail-fast`](../manifest/#test) |
| `--include-tag <tag>` | Only run JVM tests with this JUnit `@Tag`. Repeatable. See [Tags](../../guides/testing/#tags) |
| `--exclude-tag <tag>` | Skip JVM tests with this JUnit `@Tag`. Repeatable. See [Tags](../../guides/testing/#tags) |
| `--failed` | Rerun only the JVM test classes that failed in the last run. Conflicts with `--list` and `--no-run`. See [Rerunning failures](../../guides/testing/#rerunning-failures) |
| `--against-min` | Run JVM tests with the R8-optimized jar on the runtime classpath; useful for finding missing keep rules |
| `--report junit` | Also write JUnit XML reports to `target/<profile>/<target>/test-reports/` for CI systems. See [Test reports for CI](../../guides/testing/#test-reports-for-ci) |
| `--debug[=<port>]` | Start the JVM tests suspended until a debugger attaches over JDWP to `127.0.0.1:<port>` (default `5005`). Needs `-t jvm` in a project with several targets. See [Debugging tests](../../guides/testing/#debugging-tests) |
| `--list` | Compile the tests and print them (`<target>: <Class>.<name>`) instead of running them. JVM and Native only. See [Listing tests](../../guides/testing/#listing-tests) |
| `--format <fmt>` | With `--list`: `text` (default) or `json`, one [`test-discovered`](../machine-readable-output/#test-discovery) object per test |
| `--message-format <fmt>` | `human` (default) or `json`: compiler diagnostics and each test's start and result as [JSON messages](../machine-readable-output/#test-results) on stdout; the test report moves to stderr |
| `-r, --release` | Use the release profile |
| `--profile <name>` | Use a custom profile from kargo.toml, such as `staging`, instead of `dev` (see [Profiles](../manifest/#profilename)) |
| `-t, --target <target>` | Only this target (repeatable) |
| `--target-set <set>` | Instead of `-t`: `host` for the targets that build and run on this machine (JVM, Wasm, JS, and Native when this host's platform is in `platforms`), or `device` for the targets that run on a phone or an emulator (Android). An error when no enabled target is in the set. Ignores `[build] default-targets`. See [Target sets](../../guides/multiplatform/#target-sets) |
| `--env-file <path>` | Read this `.env` file after the standard ones, so its values win. Repeatable. Conflicts with `--no-env-file`. See [Environment variables](../../guides/environment/#choose-the-files-for-one-command) |
| `--no-env-file` | Read none of the `.env` files; the variables in kargo.toml and the shell still apply |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | Which packages to test, in dependency order. The command fails if any test failed |
| `-j, --jobs <n>` | How many packages are tested at the same time, and so how many test JVMs run at once, as for `kargo build`. Without `-j`, `KARGO_JOBS` or `[build] jobs`, one package at a time, so test output doesn't interleave |
| `--watch` | Run the tests again whenever the project's files change, until you press Ctrl+C. A failing test does not stop watching. Conflicts with `--list` and `--debug`. See [Watch mode](../../guides/watch/#rerun-tests-on-change) |
| `--clear`, `--ignore <glob>` | With `--watch`, as for [`kargo run`](#kargo-run-r) |

By default `kargo test` stops at the first target whose tests fail or don't compile, and notes which targets it skipped. See [Testing every target](../../guides/testing/#testing-every-target).

Wasm tests run on Node.js, or with `environment = "browser"` in a headless browser: Chrome, Edge, Chromium, Brave or Firefox, found on `PATH` or named by `KARGO_BROWSER`. See [Testing in a browser](../../guides/wasm/#testing-in-a-browser). JS tests run the same way, on Node.js or in a headless browser, with a runner of Kargo's own; see [JavaScript](../../guides/js/#run-and-test-with-nodejs).

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
| `--target-set <set>` | Instead of `-t`: `host` for the targets that build and run on this machine (JVM, Wasm, JS, and Native when this host's platform is in `platforms`), or `device` for the targets that run on a phone or an emulator (Android). An error when no enabled target is in the set. Ignores `[build] default-targets`. See [Target sets](../../guides/multiplatform/#target-sets) |
| `--env-file <path>`, `--no-env-file` | As for [`kargo test`](#kargo-test-t) |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | Which packages to benchmark, in dependency order |
| `-j, --jobs <n>` | How many packages are built at the same time, as for `kargo build`. Without `-j`, `KARGO_JOBS` or `[build] jobs`, one at a time, so benchmarks don't compete for the machine |

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
| `-t, --target <target>` | Only this target (repeatable); defaults to every enabled target. Documentation of a set other than the default goes to `target/debug/doc-<targets>/`, so it keeps `doc/` |
| `-p, --package <name>`, `--workspace` | Which packages to document. In a workspace, each goes to `target/debug/<name>/doc/`, and `--open` opens the first |
| `-j, --jobs <n>` | How many packages are documented at the same time, as for `kargo build`. Without `-j`, `KARGO_JOBS` or `[build] jobs`, one at a time |

Configure it with [`[doc]`](../manifest/#doc). See [API documentation](../../guides/documentation/).

## `kargo preview`

Render the `@Preview` composables of a Compose Desktop app to PNG files, into `target/preview/<package>.<function>.png`.

```sh
kargo preview [<name>...] [--list] [--width <dp>] [--height <dp>] [--density <n>] [--out-dir <dir>]
```

| Option | Meaning |
|---|---|
| `<name>...` | The previews to render, by function name (`AppPreview`) or qualified name (`com.example.AppPreview`). Default: all of them |
| `--list` | Print each preview's qualified name and `file:line`, tab-separated, without building |
| `--width <dp>`, `--height <dp>` | The size of the image in density-independent pixels (default 800 by 600) |
| `--density <n>` | Pixels per density-independent pixel (default 1; 2 for a high-density image) |
| `--out-dir <dir>` | Write the PNG files here instead of `target/preview/` |
| `-r, --release`, `--profile <name>` | Build with this profile |
| `-p, --package <name>` | The package, in a workspace |

A preview is a top-level `@Composable` function without parameters annotated with a Compose `@Preview`, in `src/` or `src-jvm/`. Needs `[frameworks] compose` and the jvm target. The images are drawn off screen, so no display is needed. See [Previews](../../guides/compose/#previews).

## `kargo fmt`

Format the Kotlin files in `src/`, `src-<target>/`, `tests/` and `tests-<target>/` with ktlint.

```sh
kargo fmt [--check]
```

| Option | Meaning |
|---|---|
| `--check` | Report unformatted code without changing files; exit with code 101 if there is any |
| `-p, --package <name>`, `--workspace` | Which packages to format |
| `-j, --jobs <n>` | How many packages are formatted at the same time, as for `kargo build`. Without `-j`, `KARGO_JOBS` or `[build] jobs`, one at a time, so each package's report stays together |

Without `--check`, files are rewritten in place, and the command exits with code 101 if ktlint found problems it can't fix. ktlint reads `.editorconfig`; configure the rest with [`[fmt]`](../manifest/#fmt). See [Formatting and linting](../../guides/formatting-and-linting/).

## `kargo lint`

Analyze the Kotlin files in `src/`, `src-<target>/`, `tests/` and `tests-<target>/` with detekt, or with `--android` an Android app with Android lint. Exits with code 101 if detekt (or Android lint) reports issues.

```sh
kargo lint [--create-baseline]
kargo lint --android [--release] [--create-baseline] [--message-format json]
```

| Option | Meaning |
|---|---|
| `--create-baseline` | Record the current issues in the baseline file (`detekt-baseline.xml` by default; `lint-baseline.xml` with `--android`) instead of reporting them |
| `--android` | Run the Android Gradle Plugin's lint on the Android target instead of detekt, configured by [`[target.android.lint]`](../manifest/#targetandroidlint). It lints the debug variant (the first flavor of each dimension), or with `--release` the release one, and copies the reports to `target/<profile>/android/lint/`. See [Lint](../../guides/android/#lint) |
| `--message-format json` | With `--android`: print each issue as a `compiler-message` JSON line on stdout, like the compilers' diagnostics |
| `-p, --package <name>`, `--workspace` | Which packages to analyze |
| `-j, --jobs <n>` | How many packages are analyzed at the same time, as for `kargo fmt` |

detekt prints each issue as `path/File.kt:line:column: message [Rule]`, with the path relative to the project root and `/` separators, like `kargo fmt --check`.

detekt reads `detekt.yml` from the project root automatically; configure the rest with [`[lint]`](../manifest/#lint). See [Formatting and linting](../../guides/formatting-and-linting/).

## `kargo add`

Add dependencies to `kargo.toml`, enable frameworks, or add a C library.

```sh
kargo add <group:artifact[:version[:classifier]]>... [options]
kargo add --path <dir> [--dev] [-t <target>] [--scope <scope>]
kargo add --npm <package[@version]>... [--dev] [-t <wasm|js>] [--types]
kargo add -F <framework,...>
kargo add --cinterop <name> [--header <file>]... [--pkg-config <package>]
```

| Option | Meaning |
|---|---|
| `--path <dir>` | Depend on the Kargo package in `<dir>` (relative to the current directory), built from source. The entry is named after that package, and its path is written relative to `kargo.toml`. See [path dependencies](../../guides/workspaces/) |
| `--dev` | Add to `[dev-dependencies]`. With `--npm`: to `[target.<target>.dev-npm-dependencies]`, the packages only `kargo test` installs |
| `-t, --target <target>` | Add to `[target.<target>.dependencies]`. With `--npm`: the `wasm` or `js` target whose npm packages to edit |
| `--npm` | Add npm packages instead of Maven dependencies (see [npm packages](#npm-packages-with-kargo-add---npm)) |
| `--types` | With `--npm`: also add `@types/<package>` for a package that ships no TypeScript declarations |
| `--scope <scope>` | `compile`, `runtime`, `provided`, `processor` (javac annotation processor) or `ksp` (KSP processor) |
| `--bom` | Import as a BOM |
| `--pre` | Allow pre-release versions when picking the latest |
| `--no-verify` | Don't check that the artifact exists (with `--npm`: that the version exists in the registry) |
| `-F, --framework <list>` | Enable built-in frameworks instead |
| `--cinterop <name>` | Add the C library `<name>` instead: a `[target.native.cinterop.<name>]` entry (with `package = "<name>"`) and, unless it exists, `interop/<name>.def`. Needs `[target.native]`. See [C interop](../../guides/c-interop/) |
| `--header <file>` | With `--cinterop`: a header the `.def` file binds (repeatable; default `<name>.h`). Warns when the header is in none of the project's include directories |
| `--pkg-config <package>` | With `--cinterop`: the pkg-config package that gives the library's flags |
| `-p, --package <name>` | The workspace member whose `kargo.toml` to edit |

`kargo add --path` refuses a dependency that would form a cycle, reuse another package's name or bring a second Kotlin version, and leaves `kargo.toml` unchanged.

When the module does not exist in any repository, `kargo add` looks for the real name in the repositories' directory listings and suggests it (`help: did you mean `org.jetbrains.kotlinx:kotlinx-coroutines-core`?`), also for a mistyped group. When the module exists but not the version, the error lists the nearby versions and marks the pre-releases among them.

Without a version, the latest release is used. Plain versions win over variants that some libraries publish next to them, such as `0.8.0-0.6.x-compat` or `33.0.0-jre`; Kargo prints a note naming the variant it skipped. When a library publishes only variants, the newest one is used.

### npm packages with `kargo add --npm`

`kargo add --npm` writes to `[target.wasm.npm-dependencies]` or `[target.js.npm-dependencies]` instead of a Maven table, in the same format-preserving way:

```console
$ kargo add --npm left-pad @js-joda/core@^3.2.0 chart.js --types
      Adding left-pad ^1.3.0 to [target.wasm.npm-dependencies]
      Adding @js-joda/core ^3.2.0 to [target.wasm.npm-dependencies]
      Adding chart.js ^4.4.1 to [target.wasm.npm-dependencies]
      Adding @types/chart.js ^4.4.1 to [target.wasm.npm-dependencies]
```

| Argument | What is written |
|---|---|
| `left-pad` | The `latest` release of the registry as a caret range, `^1.3.0` (a pre-release only with `--pre`) |
| `left-pad@1.3.0`, `left-pad@~1.2`, `left-pad@>=1 <2` | The version or range as given. An exact version must exist in the registry (unless `--no-verify`); a range must match a published version |
| `left-pad@next`, `left-pad@latest` | The version a dist-tag points at, as a caret range (`^2.0.0-beta.1`) |
| `@scope/name`, `@scope/name@^1.0.0` | A scoped package; its key is quoted in `kargo.toml` |

A project with one of the wasm and js targets writes to that target; with both, choose with `-t wasm` or `-t js`. `--dev` writes to `[target.<target>.dev-npm-dependencies]`. Adding a package that is already declared replaces its version.

Kargo asks the registry's HTTP API itself, so no package manager has to be installed. The registry is the one npm would use: `NPM_CONFIG_REGISTRY`, else the `registry` of the project's `.npmrc` and then the user's (`~/.npmrc`, or the file `NPM_CONFIG_USERCONFIG` names), else `https://registry.npmjs.org`. `@scope:registry=` lines choose the registry of a scope, and `//host/path/:_authToken=` lines (with `${VAR}` replaced) are sent as a bearer token. An unknown package is an error that names the registry. With `--offline`, the versions come from what an earlier run cached under `~/.kargo/npm/registry`, or you pass a version, which needs no lookup.

`--types` adds `@types/<package>` (`@scope/name` is `@types/scope__name`) when the chosen version ships no declarations of its own (no `types`, `typings` or `exports` `types` entry), at the same major version when the registry has it. A package without an `@types` package only gets a warning. The declarations feed the [Kotlin declarations](../../guides/wasm/#kotlin-declarations-for-npm-packages) Kargo generates.

## `kargo remove` (`rm`)

Remove dependencies or built-in frameworks from `kargo.toml`.

```sh
kargo remove <group:artifact or key>... [--dev] [-t <target>]
kargo remove --npm <package>... [--dev] [-t <wasm|js>]
kargo remove -F <framework,...>
```

| Option | Meaning |
|---|---|
| `--dev` | Remove from `[dev-dependencies]` (with `--npm`: from `[target.<target>.dev-npm-dependencies]`) |
| `-t, --target <target>` | Remove from `[target.<target>.dependencies]` (with `--npm`: from the npm packages of that target) |
| `--npm` | Remove npm packages. Without `-t`, the package is removed from the only target that declares it; its `@types/<package>` goes with it. An emptied table is removed |
| `-F, --framework <list>` | Disable built-in frameworks instead: removes each from `[frameworks]`, whether it is written as `name = true` or as a table with options, and the `[frameworks]` table when nothing is left in it. A framework that is not enabled is an error, and nothing is written |
| `-p, --package <name>` | The workspace member whose `kargo.toml` to edit |

Path dependencies are removed by their key: `kargo remove mathlib`. Without an option, the dependency is removed from `[dependencies]`.

## `kargo outdated`

List the dependencies in `kargo.toml` that have newer versions in your repositories.

```sh
kargo outdated [group:artifact or key or npm package]... [--pre]
```

```console
$ kargo outdated
name                                    current  compatible  latest   kind
org.jetbrains.kotlinx:kotlinx-datetime  0.6.0    0.6.2       0.8.0    dependencies
io.mockk:mockk                          1.13.0   1.14.11     1.14.11  dev-dependencies
left-pad                                ^1.0.0   ^1.3.0      ^1.3.0   target.wasm.npm-dependencies
```

| Column | Meaning |
|---|---|
| `current` | The version in `kargo.toml` |
| `compatible` | The newest version with the same major version (for `0.x`, the same minor version), or `-` if there is none. For an npm package, the newest version its range accepts (`~1.2.0` stays on `1.2.x`; an exact version or `^` stays on its major) |
| `latest` | The newest version, or `-` if `current` is the newest |
| `kind` | The table the dependency is declared in |

| Option | Meaning |
|---|---|
| `names` | Only check these dependencies |
| `--pre` | Consider pre-release versions. Dependencies already on a pre-release always consider them |
| `-p, --package <name>` | The workspace member to check |

A dependency stays on its variant: `33.0.0-jre` is compared with other `-jre` versions, and a plain version never moves to a `-compat` one. Dependencies declared with a version range or without a version (managed by a BOM) are skipped. The command only reads; nothing is changed.

The npm packages of `[target.wasm.npm-dependencies]`, `[target.js.npm-dependencies]` and their `dev-npm-dependencies` are compared with the registry (see [`kargo add --npm`](#npm-packages-with-kargo-add---npm) for how it is chosen). Their `latest` is what the registry's `latest` dist-tag points at, and the values show the range as `kargo upgrade` would write it (`^1.3.0`). Only a single version with an optional `^` or `~` (`1.3.0`, `^1.3.0`, `~1.3.0`) is compared; ranges such as `>=1 <2` or `1.x` are skipped.

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

Dependencies inherited with `workspace = true` are skipped; upgrade them in the workspace's `kargo.toml`. "Compatible" follows the same rule as the `compatible` column of [`kargo outdated`](#kargo-outdated). npm packages are upgraded too, keeping the `^` or `~` of the entry: `left-pad = "^1.0.0"` becomes `"^1.3.0"`, or with `--incompatible` the newest version, `"^2.0.0"`; a spec that isn't a single version is skipped (naming it says so). `kargo.lock` is re-resolved on the next build because the manifest changed.

## `kargo audit`

Check every dependency against the [OSV](https://osv.dev) database of known security advisories, and fail when one is serious enough.

```sh
kargo audit [options]
```

```console
$ kargo audit
    Auditing app v0.1.0 (42 packages, advisories from https://api.osv.dev)
severity        package          version  advisory             fixed in
critical (9.8)  com.acme:widget  1.2.0    GHSA-wwww-wwww-wwww  1.2.5
medium          com.acme:direct  1.0.0    GHSA-dddd-dddd-dddd  1.0.3

GHSA-wwww-wwww-wwww: Bad things in com.acme:widget
  aliases:  CVE-2026-1111
  url:      https://osv.dev/vulnerability/GHSA-wwww-wwww-wwww
  path:     app v0.1.0 -> com.acme:app:1.0 -> com.acme:widget:1.2.0
  found in: jvm/compile, jvm/runtime, jvm/test
...
error: 1 advisory at or above the `high` severity; upgrade the affected packages (`kargo audit --fix` raises the versions kargo.toml declares) or add the advisories to [audit] ignore with a reason
```

| Option | Default | Meaning |
|---|---|---|
| `-t, --target <target>` | every enabled target | Only these targets (repeatable) |
| `--deny <severity>` | `[audit] deny`, else `high` | The lowest severity that fails the audit: `low`, `medium`, `high`, `critical`, or `none` to report without failing |
| `--tooling` | | Also audit the build's tools (see below) |
| `--fix` | | Raise the versions `kargo.toml` declares to ones that fix the advisories |
| `--format <fmt>` | `text` | `text` or `json` |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | current package | Which packages to audit |

What is audited:

- **Maven modules**: the compile, runtime and test classpaths of the JVM and the main and test resolutions of Wasm, JS and every Native platform, resolved as a build would (with `--locked`, from `kargo.lock` only). The `path` line is the shortest chain of dependencies that brings the module in, as `kargo tree` shows it.
- **npm packages** of Wasm and Kotlin/JS: every package installed in the target's `node_modules` (the ones the libraries ask for, `npm-dependencies` and what they pull in), with the library or package that asks for it in the path. A package that is not installed yet is audited at its declared version when that is exact, and skipped with a note when it is a range; build the target once to audit what it installs.
- **The build's tools**, with `--tooling`: the Kotlin compiler, KSP, the JUnit launcher, compiler plugins, `[[generate]]` tools and the benchmark generator as `kargo.lock` holds them (run `kargo fetch --tools` or a build first, so they are locked), and the npm packages of the TypeScript compiler. They are marked `(build tooling)`.
- Android dependencies are resolved by Gradle and are not audited.

Severity comes from the advisory's CVSS v3 vector (the base score is shown next to it), else from the severity the advisory database gives (GitHub's `moderate` is `medium`). An advisory with neither is `unknown`, and fails at every `--deny` level but `none`. `fixed in` is the lowest version above yours that fixes the advisory, or `-` when no fix is published.

Exit codes: 0 when nothing at or above `--deny` is found (advisories below it print a warning), 101 when something is. Advisories listed in [`[audit] ignore`](../manifest/#audit) are left out of the table and the exit code, and printed as notes with their reason; an entry past its `until` date stops applying, with a warning.

`--format json` prints one object per package (an array for several): `package`, `database`, `deny`, `audited` (how many package versions were checked), `skipped` (notes about packages that could not be checked) and `advisories`, each with `id`, `aliases`, `summary`, `severity` (`null` when unknown), `score`, `url`, `ecosystem` (`maven` or `npm`), `package`, `version`, `fixed`, `path`, `resolutions`, `tooling`, `ignored` (`null`, or the `reason` and `until` of the entry) and `denied` (whether it fails the audit).

`--fix` changes the version of each vulnerable dependency that `kargo.toml` declares to the lowest version that fixes all of its advisories, when that version is compatible with the declared one (the rule of [`kargo upgrade`](#kargo-upgrade)); a caret or tilde range of an npm package keeps its prefix. For a package that comes in through another dependency, it says which one to upgrade instead (or, for a Maven module, that declaring it at the fixed version overrides it, since the highest version wins). Inherited, BOM-managed and range versions are reported, not changed. The exit code is still the audit's; run `kargo audit` again to check the result.

:::note
`kargo audit` is the only command that contacts the advisory database. It sends the names and versions of your dependencies to `https://api.osv.dev` (or to `KARGO_OSV_URL`) and nothing else. The answers are cached under `~/.kargo/advisories` for a day; `--offline` audits from the cache whatever its age (and fails for a package that was never looked up), and `--refresh` ignores it.
:::

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
| `<target>...` | `jvm`, `wasm`, `js`, `native` or `android`; space- or comma-separated |
| `--lib` | `add`: a Wasm, JS, Native or Android library (`kind = "library"`) |
| `--app` | `add`: a Wasm or JS application, a Native executable or an Android application |
| `--format json` | `list`: print JSON (`enabled` with each target's settings, and `available`) |
| `-p, --package <name>` | The workspace member whose `kargo.toml` to edit or list |

`kargo target add`:

- Writes the defaults of `kargo new`: `environment = "js"` for Wasm, `environment = "node"` for JS, `platforms = ["host"]` and a `kind` for Native, and `namespace`, `kind` and `min-sdk = 24` for Android (the namespace is the package's `group` plus its name). Wasm and JS get `kind = "library"` for a library, as `--lib` does. With `[frameworks] compose`, Wasm gets `environment = "browser"` and Native the iOS platforms with `kind = "framework"`, as [Compose](../../guides/compose/) needs.
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
| `-d, --duplicates` | | Only modules requested in several versions. Prints `no duplicate modules` when there are none |
| `-i, --invert <module>` | | Show what depends on `<module>` (`group:artifact`, optionally `:version`), up to the project: the inverted tree |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | current package | Which packages to show |
| `--format <fmt>` | `text` | `text` or `json`. `json` prints the whole resolved graph, with each module's version, files and direct dependencies (and for wasm and js the `npm` packages, see below); it can't be combined with `--depth`, `--duplicates` or `--invert`. When several packages are selected, the JSON output is an array with one document per package |

Path dependencies are listed first, as `mathlib v0.1.0 (<directory>)`, each with the path dependencies it uses in turn (the module graph); their Maven dependencies appear in the graph next to yours. With `-c compile`, only the path dependencies there are to compile against are listed: a path dependency's own path dependencies are only there at runtime unless they are `exported` ([what comes along](../../guides/workspaces/#what-comes-along)).

`--invert` turns the tree upside down: the module first, then every module that depends on it, down to the project. A module that asked for another version than the selected one says which (`requests g:util:1.0`). With a version (`g:util:1.0`), the module is shown when that version is the selected one or one that something requested. `--depth` limits how many levels are shown. The command fails when no resolution has the module.

```console
$ kargo tree --invert g:util
g:util:2.0
├── g:left:1.0 (requests g:util:1.0)
│   └── app v0.1.0 (jvm)
└── g:right:1.0
    └── app v0.1.0 (jvm)
```

For the wasm and js targets, the tree is followed by the `npm packages` the target installs, each with the version that wins and where it comes from: the Maven libraries whose klibs ask for it (with the version they want when it differs), and `kargo.toml`, which overrides them. `-c test` adds the `dev-npm-dependencies`.

```console
$ kargo tree -t wasm
web v0.1.0 (wasm-js)
├── org.jetbrains.kotlin:kotlin-stdlib-wasm-js:2.2.20
└── org.jetbrains.kotlinx:kotlinx-datetime-wasm-js:0.7.1
npm packages
├── @js-joda/core 3.2.0 (asked for by org.jetbrains.kotlinx:kotlinx-datetime-wasm-js:0.7.1)
└── left-pad ^1.3.0 (kargo.toml)
```

In `--format json`, each tree has an `npm` array of `{ name, version, declared, dev, requested-by }` objects. The packages of a path dependency are not listed: its klibs are only built by `kargo build`.

## `kargo why`

Explain why a module is on the classpath, and why at its version.

```console
$ kargo why g:util
g:util:2.0 (jvm, runtime)
  selected 2.0, the highest version requested
  requested:
    1.0 -> 2.0  by g:left:1.0 (highest wins)
    2.0         by g:right:1.0
  path:
    app v0.1.0 -> g:left:1.0 -> g:util:2.0
```

The module is `group:artifact`, optionally with a version: `g:util:1.0` explains `g:util` when `1.0` is its version or one that something requested. Each resolution that has the module gets one answer (one per Native platform). The command fails when none has it.

The `selected` line says what decided the version:

| Reason | Meaning |
|---|---|
| `the highest version requested` | Several versions were requested and the highest won, as in Gradle |
| `the only version requested` | Every request asked for the same version |
| `forced in kargo.toml` | A dependency with [`force = true`](../manifest/#dependencies-and-dev-dependencies) |
| `the highest version that satisfies <ranges>` | A request was a Maven range |
| `locked in kargo.lock` | `kargo.lock` holds this version, and resolving again would pick another one; the line says which and why. `kargo update <group:artifact>` takes the new one |

`requested` lists every version asked for in the resolved graph, and who asked: `kargo.toml` (a dependency you declare, `force = true` noted), another module (`g:left:1.0`), a BOM (`BOM g:bom:1.0`), a platform another module depends on (`platform g:bom:1.0 of g:api:1.0`), a Gradle module metadata constraint (`(dependency constraint)`), a module whose POM relocates to this one (`(relocated)`) or a Kotlin Multiplatform module whose variant for the target is this one (`(Gradle module metadata variant)`). A rejected version shows as `1.0 -> 2.0` with what it lost to. Requests from modules that are no longer in the graph (an older version's dependencies) are not listed.

`path` is the shortest chain of dependencies from `kargo.toml` to the module; `--all-paths` lists every one, shortest first (at most 100).

| Option | Default | Meaning |
|---|---|---|
| `-t, --target <target>` | every enabled target but Android | Which target |
| `-c, --configuration <c>` | `runtime` | `compile`, `runtime` or `test` |
| `--all-paths` | | Every path from the project to the module, not only the shortest |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | current package | Which packages to explain |
| `--format <fmt>` | `text` | `text` or `json` ([format](../machine-readable-output/#kargo-why---format-json)) |

The classpath is the one a build uses, from `kargo.lock`; the reasons come from resolving the same dependencies again, without the lock and without downloading artifacts (the POMs are in the cache already). Android dependencies are resolved by Gradle: run `kargo android -- dependencyInsight --configuration releaseRuntimeClasspath --dependency <module>` there.

## `kargo inspect`

Show where the jar of a resolved dependency is, and what is in it: the `javap` of a dependency, without finding its jar in the cache.

```console
$ kargo inspect g:util
  Inspecting g:util:2.0 (jvm, runtime) /home/me/.kargo/cache/maven/g/util/2.0/util-2.0.jar
com.acme.util.Strings
com.acme.util.Strings.Companion
note: 2 classes; name one to print its members: `kargo inspect g:util <class>`

$ kargo inspect g:util Strings
  Inspecting g:util:2.0 (jvm, runtime) /home/me/.kargo/cache/maven/g/util/2.0/util-2.0.jar
public final class com.acme.util.Strings {
  public static final com.acme.util.Strings$Companion Companion;
  ...
}
```

The module is `group:artifact`, optionally with a version (`g:util:2.0`; it must be the one on the classpath or one something requested). It is looked up in the resolution a build uses, so the jar is the version `kargo.lock` pins. A module that only redirects to another (a Kotlin Multiplatform root such as `kotlinx-coroutines-core`) shows the jar of the module it redirects to. The status line, on stderr, says which module, resolution and jar.

With no class, the public classes of the jar are listed on stdout. Anonymous and local classes, `module-info` and `package-info` are left out. With one or more classes, their members are printed by the JDK's `javap` (the JDK of `[toolchain] jdk`), with the whole classpath behind them so supertypes resolve. A class is named in full (`com.acme.Outer.Inner` or `com/acme/Outer$Inner`) or by the end of its name (`Inner`, `Outer.Inner`); the command fails when no class or several classes of the jar match, and lists the several.

The output is the JVM view of a class: a Kotlin property shows as a getter and a setter, a top-level function as a static method of its `FooKt` class.

| Option | Default | Meaning |
|---|---|---|
| `-c, --configuration <c>` | `runtime` | `compile`, `runtime` or `test` |
| `--visibility <v>` | `public` | Members to print: `public`, `protected`, `package` or `private`. `package` and `private` also list the classes that are not public |
| `-p, --package <name>` | current package | Which package's classpath |

Only the JVM target has jars. For the files of the whole classpath, use [`kargo classpath`](#kargo-classpath).

## `kargo classpath`

Print the resolved classpath (JVM) or klib path (Wasm, JS, Native) of a target: the jars on disk, where `kargo tree` shows coordinates. Use it to point any tool, script or editor at a project's libraries.

```console
$ kargo classpath --format lines
/home/me/.kargo/cache/maven/io/ktor/ktor-server-core-jvm/3.2.3/ktor-server-core-jvm-3.2.3.jar
/home/me/.kargo/cache/maven/org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21.jar
...
```

The entries are the ones the build compiles with, in the same order: path dependencies first (built if needed), then the resolved Maven artifacts, including the Kotlin stdlib and what [frameworks](../../guides/frameworks/) add. The package's own classes are not included. Resolution follows `kargo.lock`, `--locked` and `--offline`, and may download dependencies and update `kargo.lock`, as `kargo tree` does.

| Option | Default | Meaning |
|---|---|---|
| `-t, --target <target>` | `jvm` | Which target. Without it, the JVM target, or the package's only Wasm, JS or Native target. A Native target prints the klibs of its first platform; Android's classpath comes from Gradle (`kargo android -- dependencies`) |
| `-s, --scope <scope>` | `compile` | `compile` (what the main code compiles against), `runtime` (what it runs with) or `test` (what the tests compile and run against, dev-dependencies included) |
| `--format <fmt>` | `path` | `path`: one line joined with the platform's path separator (`:`, or `;` on Windows), ready for `-cp`. `lines`: one entry per line. `json`: an object with `package`, `target`, `platform`, `scope` and `classpath` |
| `-p, --package <name>` | current package | Which package |

## `kargo package`

Build a self-contained, runnable fat jar with all runtime dependencies, path dependencies included: `target/<profile>/jvm/<name>-<version>-all.jar` (`target/<profile>/<name>/jvm/` in a workspace). The path of the jar is printed on stdout.

With `--release` (or `--optimize`), Kargo also shrinks that jar with R8 into `<name>-<version>-min.jar` and prints the path of the `-min.jar` instead. Both jars stay in the directory:

| Jar | What it is | Use it when |
|---|---|---|
| `<name>-<version>-all.jar` | The fat jar as built, every dependency class included | You want the program exactly as compiled, or the shrunk jar fails because of a class loaded by reflection |
| `<name>-<version>-min.jar` | The same jar after R8 removed unused code and renamed what is left, using the rules of [`[package.optimize]`](../manifest/#packageoptimize) and `proguard-rules.pro` | You ship a release and have run it to check that the keep rules cover your app |

See [JVM shrinking and obfuscation](../../guides/publishing/#jvm-shrinking-and-obfuscation).

`--format` ships a JVM application another way. Each prints the path of what it built on stdout:

| Format | Builds |
|---|---|
| `jar` | The fat jar above (the default) |
| `jlink` | `target/<profile>/jvm/package/<name>-<version>/`: the jars in `lib/`, a Java runtime with only the JDK modules the program uses in `runtime/`, and `bin/<name>` and `bin/<name>.bat` launchers. Runs on machines without Java |
| `native-image` | `target/<profile>/jvm/package/<name>` (`<name>.exe` on Windows): a native executable built by GraalVM's `native-image` |
| `installer` | An installer built by `jpackage` into `target/<profile>/jvm/package/`: `.msi` on Windows, `.dmg` on macOS, `.deb` on Linux, or the [`[package.installer]`](../manifest/#packageinstaller) `type` |

`--dist` builds an application distribution instead: an archive with `bin/` start scripts and the jars in `lib/`, for machines that already have Java. It writes `target/dist/<name>-<version>.zip` and `.tar.gz` (`--archive zip`, `tar` or `tar.gz`, comma separated, picks some), prints their paths and leaves the unpacked tree in `target/dist/<name>-<version>/`. See [Distribution archives](../../guides/distributing/#distribution-archives).

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
| `--dist` | Build an [application distribution](../../guides/distributing/#distribution-archives): `bin/` start scripts and `lib/` jars in a zip and a tar.gz (JVM). Does not combine with `--format`, `--fat`, `--optimize` or `--desktop`; `--bin` limits it to one start script |
| `--archive <formats>` | With `--dist`: `zip`, `tar` or `tar.gz`, comma separated (default `zip,tar.gz`) |
| `--no-scripts` | With `--dist`: leave out the start scripts (a distribution of jars, for a library or your own scripts) |
| `--optimize` | Shrink and obfuscate the JVM jar with R8 into `-min.jar`, next to the `-all.jar`; also enabled automatically with `--release` |
| `--message-format json` | Print compiler diagnostics, optimization reports and package paths as JSON lines |
| `--desktop` | Package the [Compose Desktop](../../guides/compose/#distributing) application: an installer for this OS unless `--format` says otherwise, with the JDK modules, options and Skiko native library Compose needs. Needs `[frameworks] compose` |
| `--platforms <list>` | With `--desktop --format jar`: the platforms whose Skiko natives the jar carries instead of only the host's, so it runs on each of them. `all`, or some of `windows-x64`, `windows-arm64`, `macos-x64`, `macos-arm64`, `linux-x64`, `linux-arm64` |
| `-j, --jobs <n>` | How many targets and Native platforms build at the same time, as for [`kargo build`](#kargo-build-b) |

## `kargo retrace`

```sh
kargo retrace target/release/jvm/app-0.1.0-mapping.txt crash.txt
```

Uses the mapping file produced by optimized packaging to restore obfuscated stack frames. If the stack trace path is omitted, Kargo reads it from stdin.

## `kargo publish`

Build a library with the release profile and publish it to a Maven repository: the local one (`~/.m2/repository`), a [`[repositories]`](../manifest/#repositories) entry, or Maven Central. A Wasm or Kotlin/JS library can also be published to an npm registry with `--npm`. See [Publishing a library](../../guides/publishing/).

```sh
kargo publish [--repository <name> | --local] [--dry-run]
kargo publish --npm [--registry <url>] [--tag <tag>] [--otp <code>] [--dry-run]
```

Every target is published, Android only for a library (`kind = "library"`): a JVM-only library as one Maven module (`<name>.jar`), an Android-only library as one module with the AAR of its release build (`<name>.aar`), anything else as a multiplatform library with a root module and one module per platform (`<name>-jvm`, `<name>-android`, `<name>-wasm-js`, `<name>-js`, `<name>-linuxx64`, ...). See [Android libraries](../../guides/publishing/#android-libraries). Each module gets a POM, Gradle module metadata, a sources jar and a javadoc jar, the checksums `.md5`, `.sha1`, `.sha256` and `.sha512`, and `.asc` signatures when [signing](../../guides/publishing/#signing) is on. Everything is staged in `target/release/publish/repository` first. A package with `kind = "bom"` in [`[publish]`](../manifest/#publish) is published as a [BOM](../../guides/publishing/#boms), a POM only, without compiling anything, and each entry of `relocations` gets a [relocation POM](../../guides/publishing/#renamed-artifacts).

| Option | Meaning |
|---|---|
| `--repository <name>` | Where to publish: a `[repositories]` entry, `maven-central` or `maven-local`. Defaults to `repository` in [`[publish]`](../manifest/#publish) |
| `--local` | Publish to the local Maven repository, like `--repository maven-local` |
| `--dry-run` | Build, stage and sign everything, list each module with its files and their SHA-256 (npm: pack the package and list its files with their sizes), but upload nothing |
| `--npm` | Publish only the npm package of a Wasm or Kotlin/JS library, not to Maven. Cannot be used with `--repository`, `--local` or `--maven` |
| `--maven` | Publish only to Maven, even when [`[publish.npm]`](../manifest/#publishnpm) is set |
| `--registry <url>` | With `--npm`: the registry. Defaults to `registry` in `[publish.npm]`, then your `.npmrc`, then `https://registry.npmjs.org` |
| `--tag <tag>` | With `--npm`: the dist-tag of the version, such as `next`. Defaults to `tag` in `[publish.npm]`, else `latest` |
| `--otp <code>` | With `--npm`: a one-time password, for registries that ask for one |
| `-p, --package <name>`, `--workspace` | Which packages to publish. Path dependencies are published before the packages that use them; members with `publish = false` are skipped when the whole workspace is published |

The package needs a `group` in `[package]`. Maven Central also needs `description`, `license`, `authors` and `repository`, a release (non-SNAPSHOT) version, and signatures; Kargo checks them before building.

A package with [`[publish.npm]`](../manifest/#publishnpm) publishes to npm too, after Maven: `kargo publish` runs both, and `--npm` or `--maven` picks one. `--npm` builds the library with the release profile and publishes the directory with the `package.json` Kargo writes (`target/release/wasm` or `target/release/js`) with npm, pnpm or bun: the `package-manager` of the target, else the first of the three on `PATH`. Before anything is built it checks the package name and version (a pre-release needs a dist-tag), that the registry does not have the version yet (also with `--dry-run`, which fails on a published version), and the provenance setup (`provenance = true` is refused with bun, which can't sign); afterwards that every file `package.json` points at exists. The tarball is written to `target/release/npm/<name>/` and its files are listed, so `--dry-run` shows exactly what a publish uploads. In a workspace, `--npm` skips the members that build no npm package, and members are published after the ones they refer to. The token comes from [`NPM_TOKEN`](../environment/) or your `.npmrc`, never from `kargo.toml`. See [Publishing to npm](../../guides/publishing/#publishing-to-npm).

A release already in the destination repository is not uploaded again: when its files match (by SHA-1), the module is skipped and the command succeeds; when they differ, it fails and asks for a new version. SNAPSHOT versions and the local Maven repository are overwritten.

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

Download all dependencies and toolchains so later builds can run offline, and lock them in `kargo.lock`. For each enabled target this is the Kotlin compiler, its plugins and the dependencies; the compiler, the plugins and the JUnit launcher are pinned as [build tools](../lockfile/#build-tools), and so is the [TypeScript compiler](../../guides/toolchains/#the-typescript-compiler) of the Wasm and JS targets when the package has `.ts` modules. The JVM, Wasm and JS targets also get the JDK that `[toolchain] jdk` asks for (`system`, the default, is the JDK on the machine and is left alone). The JVM target gets the JUnit console launcher that `kargo test` runs and, in a package with `benches/`, the libraries and the harness generator of [`kargo bench`](#kargo-bench) (locked as `jvm/bench` and `jvm/bench-tools`), and the Wasm and JS targets get the [npm packages](../../guides/wasm/#npm-packages) of their Maven dependencies when npm, pnpm or bun is on `PATH`; without one, Kargo warns and skips them. For Android this is the [Gradle distribution](../../guides/android/#gradle) and its wrapper; Gradle fetches Android dependencies itself during the build.

| Option | Meaning |
|---|---|
| `-t, --target <target>` | Only this target (repeatable) |
| `--sources` | Also download the `-sources.jar` of every resolved library (JVM and klib targets), so editors can show and step into library code. For a library that publishes none, download its `-javadoc.jar` instead, so editors still [show its documentation](../../guides/kotlin-server/#documentation-from-javadoc-jars). Libraries with neither are skipped. Sources and Javadoc jars are not recorded in `kargo.lock` |
| `--tools` | Also download and lock the tools of [`kargo doc`](../../guides/documentation/) (Dokka), [`kargo fmt`](../../guides/formatting-and-linting/) (ktlint), [`kargo lint`](../../guides/formatting-and-linting/) (detekt) and R8 for JVM optimization |
| `-p, --package <name>`, `--workspace` | Which packages to fetch for |

```console
$ kargo fetch --sources
 Downloading kotlin-stdlib-2.4.21-sources.jar
     Fetched 12 sources jars and 2 javadoc jars
```

The count of javadoc jars is left out when there are none.

When the JVM target is fetched, `kargo fetch` also prints a `Resolved` line for each enabled framework with its version, as [`kargo build`](#kargo-build-b) does, so you can see which Spring Boot, Spring Framework and Jackson versions the project uses before you build.

## `kargo generate`

Run the [`[[generate]]` steps](../manifest/#generate) of kargo.toml without building: protobuf, OpenAPI, `BuildConfig` and other generated sources. `kargo build`, `check`, `test`, `run` and `package` run the steps they need first, so this command is for CI checks, for trying a step out, and for an editor that wants the generated files before the first build.

```sh
kargo generate [NAME]... [-t <target>] [--release | --profile <name>] [--force]
```

| Option | Meaning |
|---|---|
| `NAME` | Only these steps, and the steps they wait for (`after`). Default: every step |
| `-t, --target <target>` | Only the steps that compile for this target (repeatable) |
| `--force` | Run steps whose output is up to date too |
| `-r, --release`, `--profile <name>` | Generate the output of this profile (`target/generated/<profile>/`) |
| `-p, --package <name>`, `--workspace`, `--exclude <name>` | Which packages to generate for |

A step with `inputs` runs only when they, its definition or Kargo changed since it last succeeded; one that is up to date is reported as `Fresh`. Without any `[[generate]]` step, the command fails; in a workspace, members without steps are skipped. See [Code generation](../../guides/code-generation/).

```console
$ kargo generate
  Generating `protos` with protoc
$ kargo generate
       Fresh `protos`: the generated files are up to date
```

## `kargo vendor`

Copy every Maven file a build of the project needs into a directory laid out as a Maven repository (`group/artifact/version/...`, with POMs, `.module` files, `maven-metadata.xml` and `.sha1` checksums), so a build with `--vendored` resolves from it alone, for example on a machine without network access. It copies what [`kargo fetch`](#kargo-fetch) downloads for the enabled targets, and every file of `kargo.lock`, checked against its SHA-256. The Android target is skipped, since Gradle resolves its dependencies. See [Offline and corporate builds](../../guides/offline-and-mirrors/).

```sh
kargo vendor [path] [--toolchains] [--sync]
```

| Option | Meaning |
|---|---|
| `path` | The directory to write, relative to the project root (default: `[source] directory` of `.kargo/config.toml`, else `vendor`) |
| `--toolchains` | Also copy the Kotlin compiler and the Kotlin/Native distribution for this host. Without it, the compiler's `tools/kotlin-compiler` entry of `kargo.lock` is not copied |
| `--sync` | Remove the files of the directory that the project no longer needs |

```console
$ kargo vendor
    Vendored 214 files into /home/me/app/vendor (214 copied)
note: to build from it, pass `--vendored`, or add this to .kargo/config.toml:

[source]
vendored = true
```

## `kargo update`

Re-resolve dependencies and rewrite `kargo.lock`. Cached repository metadata is ignored, so new versions within version ranges and new builds of [SNAPSHOT versions](../../guides/dependencies/#snapshot-versions) are picked up. The [build tools](../lockfile/#build-tools) that `kargo.lock` had are locked again too, so `--locked` builds still find them; a tool it did not have is locked by the first build that runs it. A bare `kargo update` also looks up the newest [TypeScript compiler](../../guides/toolchains/#the-typescript-compiler) again (unless `[toolchain] typescript` names an exact version); `kargo update <group:artifact>` keeps the one that is locked.

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

Delete everything in `target/` except the build-directory lock, `target/.kargo-lock` (and its `.kargo-lock.holder`), which includes the files of [`[[generate]]` steps](../../guides/code-generation/) in `target/generated/`. In a workspace, that is the workspace's `target/`.

| Option | Meaning |
|---|---|
| `-r, --release` | Only delete the release profile's output, with its generated files (`target/generated/release/`) |
| `--profile <name>` | Only delete the output of a custom profile (`target/<name>/`, `target/generated/<name>/`), even one that kargo.toml no longer defines |

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

List tasks, or run one. Extra arguments are appended to the task's last command, or are the `args` of a task written as a [Kotlin script](../../guides/tasks-and-plugins/#kotlin-scripts) (`script = "..."`), which `kargo script` runs. The listing shows a script task as `kargo script <file>` when it has no description.

```sh
kargo task               # list
kargo task --list        # the same
kargo task [--env-file <path>] [--no-env-file] <name> [args]
kargo <name> [args]      # shorthand
```

A first `--` after the task name is dropped in both forms, so `kargo task <name> -- <args>` and `kargo <name> -- <args>` pass the same arguments; a second `--` is passed on (`kargo say -- -- a` gives the task `-- a`).

A package task starts with the [project environment](../../guides/environment/): `--env-file <path>` (repeatable) and `--no-env-file` choose its `.env` files, as they do for `kargo run`. They can come before the name or right after it, where they are taken from the front of the arguments: `kargo deploy --env-file .env.staging --dry-run` passes only `--dry-run` to the task. Workspace tasks run at the root and get no project environment.

In a workspace, a member's directory runs that member's tasks, and the root runs a `[workspace.tasks]` task; a name that only members define runs in every member that has it, in dependency order. See [Workspace tasks](../../guides/workspaces/#tasks-and-plugins).

Inside a project, `kargo --help` also lists its tasks. A task named like a built-in command, such as `build` or `test`, can only run as `kargo task <name>`; the listing marks it and the commands that work on a package [warn about it](../warnings/#tasks-hidden-by-built-in-commands).

## `kargo plugins`

List hook plugins (with the executable or `script` each runs), compiler plugins and `kargo-*` subcommands.

## `kargo android`

Run Gradle in the generated Android project (`target/android`), and manage the devices, emulators and virtual devices that `kargo run -t android` uses. See [Devices and emulators](../../guides/android/#devices-and-emulators).

```sh
kargo android <gradle args...>
kargo android lint
kargo android -s emulator-5554 connectedAndroidTest
kargo android devices
```

| Subcommand | Meaning |
|---|---|
| `<gradle args...>` | Run the Gradle tasks. A task that uses a device (`install*`, `uninstall*`, `connected*`, `*AndroidTest`) runs on the device `-s, --device` (or `KARGO_ANDROID_DEVICE`) names, else the one Kargo chooses like `kargo run` does |
| `devices [--format text\|json]` | List physical devices, running emulators and installed virtual devices with their id, name, type, API level and state (`device`, `unauthorized`, `offline`, `no permissions`, `stopped`). `kargo devices` is the same |
| `avd list` (`ls`) `[--format text\|json]` | The installed virtual devices |
| `avd create [name] [--api <level>] [--hardware <profile>] [--force] [--accept-licenses]` | Create a virtual device with `avdmanager` (default name `Kargo_API_<api>`, default API the project's `compile-sdk`). With Kargo's SDK it installs the emulator, the command-line tools and the system image first, after telling you the download and asking |
| `avd delete` (`rm`) `<name>` | Delete a virtual device that is not running |
| `emulator start [avd] [--headless] [--cold-boot] [--wipe-data] [--boot-timeout <seconds>]` | Start a virtual device (the only one without a name) and wait until it booted |
| `emulator stop <serial\|avd>` | Shut down a running emulator |
| `connect <host:port>` | Connect to a device over Wi-Fi (wireless debugging) |
| `pair <host:port> [code]` | Pair with a device for wireless debugging; `adb` asks for the code when it is left out |
| `disconnect [host:port]` | Disconnect from one wireless device, or all of them |

The subcommands work outside a project too, with the SDK of `ANDROID_HOME` (or Android Studio's location), else Kargo's.

## `kargo toolchain`

Manage JDKs, GraalVMs, Kotlin/Native and Gradle distributions, the Android SDK, and the Kotlin language servers [`kargo lsp`](#kargo-lsp) runs.

| Subcommand | Meaning |
|---|---|
| `list` (`ls`) | Installed toolchains, disk usage and what this project uses, including whether the Kotlin/Native dependencies of each native platform are downloaded |
| `install [jdk\|graalvm\|native\|gradle\|android-sdk\|kotlin-lsp\|kls\|kotlin-debug-adapter] [version] [--accept-licenses] [--emulator]` | Install a toolchain; with no arguments, whatever the project needs (with `jdk = "system"` and no JDK on `JAVA_HOME` or `PATH`, the JDK a build would install), including the Kotlin/Native dependencies (LLVM, sysroots) of every `[target.native]` platform this host can build, so native builds work `--offline` afterwards. It installs even with `KARGO_AUTO_INSTALL=0`. `graalvm` takes `25` (GraalVM Community Edition) or `oracle-25` (Oracle GraalVM). `gradle` without a version installs the one the project uses; `android-sdk` installs a platform API level (default: the project's [`[toolchain] android-sdk`](../manifest/#toolchain)) with the build-tools of the project's AGP and platform-tools, after showing Google's license terms and asking, or accepting them with `--accept-licenses` (for CI; see [Android SDK](../../guides/android/#android-sdk)); `--emulator` adds the emulator, the command-line tools and a system image for that API level, to run apps on a virtual device; `kotlin-lsp` and `kls` without a version install the one `kargo lsp` uses, `kotlin-debug-adapter` the one `kargo dap` uses |
| `remove` (`rm`) `<jdk\|graalvm\|native\|gradle\|android-sdk\|kotlin-lsp\|kls\|kotlin-debug-adapter> <version>` | Remove one. For JDKs and GraalVMs, a major version removes all its releases. For `android-sdk`, an API level removes that platform, and a package path such as `build-tools;36.0.0` that package; the SDK of `ANDROID_HOME` is never touched |
| `available [jdk\|android-sdk]` | JDK versions available for download (LTS marked), or the Android SDK platforms and build-tools |
| `which` | The toolchains this project resolves to |

Other commands install the toolchains they need on first use, including a JDK when the project uses the system JDK and the machine has none; `KARGO_AUTO_INSTALL=0` or `[toolchain] auto-install = false` in `config.toml` turns that off. See [what installs automatically](../../guides/toolchains/#what-installs-automatically).

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

**The update notice.** `kargo --version` looks the newest version up at most once a day (with a two-second timeout; the answer is remembered in `~/.kargo/update-check.json`) and, when it is newer than the running one, prints a note on stderr: ``note: update available: kargo 0.1.4 (this is kargo 0.1.2); run `kargo self update` ``, also at most once a day. Nothing is printed or fetched with `--offline`, with `-q`, when `KARGO_NO_UPDATE_CHECK` is set, in CI (`CI` is set) or when stderr is not a terminal. No other command checks.

## `kargo daemon`

Show or stop the background compiler daemons that keep the Kotlin compiler warm between builds. See [Build performance](../../guides/build-performance/#the-compiler-daemon).

| Subcommand | Meaning |
|---|---|
| `status` | List running daemons with their process ID and state directory |
| `stop` | Stop every running daemon; the next build starts a new one |

## `kargo doctor`

Check the machine and the project for what keeps builds from working, and say how to fix each problem.

```console
$ kargo doctor
Kargo
  ok       version: kargo 0.1.4 (x86_64-unknown-linux-gnu)
  ok       KARGO_HOME: /home/me/.kargo (writable)
  ok       disk space: 120.4 GiB free
  ok       compiler daemons: none running

JDK
  ok       project JDK: JDK 25 at /usr/lib/jvm/jdk-25 (system), runs Kotlin 2.4.21

Toolchains
  ok       installed: JDK jdk-21.0.12+7, Gradle 9.6.0
  warning  JDK jdk-17.0.20+8: JDK jdk-17.0.20+8 at /home/me/.kargo/toolchains/jdk/jdk-17.0.20+8 is incomplete (an install was interrupted)
           fix: `kargo doctor --fix` removes it; it is installed again when needed
  ok       Kotlin: 2.4.21 (downloaded from Maven on first build)

Network
  ok       proxy: none
  ok       repository maven-central: https://repo.maven.apache.org/maven2 (HTTP 200)

Project
  ok       package app: /home/me/app/kargo.toml loads without warnings
  ok       kargo.lock: in sync with kargo.toml

0 errors, 1 warning.
```

Each check is `ok`, a `warning` (something to know, or a build that will download what it needs) or an `error` (a build that will fail), with a `fix:` line for the last two. Outside a project, `kargo doctor` checks only the machine; inside one, it checks what the project needs too. The Android SDK, Node.js and a package manager are listed either way: when the project does not use them (or there is no project), a missing one is reported as `ok` with a note on what needs it, and when the project does, it is a warning or an error.

| Section | What it checks |
|---|---|
| Kargo | The version and platform, that the Kargo home is writable, the free disk space there (a warning under 5 GiB, an error under 1 GiB), the running [compiler daemons](#kargo-daemon), and toolchain installs another Kargo process holds the lock of |
| JDK | `JAVA_HOME` (one without `bin/java`, or another JDK than `java` on `PATH`), and the JDK the project builds with: found or [pinned and installed](../../guides/toolchains/), at least JDK 11, one the project's Kotlin compiler runs on, new enough for `jvm-target`, with `javac`. A pinned `[toolchain] graalvm` too |
| Toolchains | The JDKs, GraalVMs, Kotlin/Native and Gradle distributions, language servers and debug adapter installed under the Kargo home, and installs that were interrupted (a directory without a complete toolchain). With the Native target, whether its Kotlin/Native is installed, and a warning for each platform in `platforms` this host cannot build (Apple platforms off macOS) |
| Android | The Android SDK the machine has, and with the Android target: the SDK (`ANDROID_HOME`, or the [Kargo-managed one](../../guides/android/#android-sdk)), its platform and build-tools, accepted licenses, `adb` and its version, the connected devices and their states, the installed virtual devices, the emulator package and whether the machine accelerates the emulator (KVM, the Windows Hypervisor Platform, Hypervisor.framework), and the Gradle the build runs |
| Wasm and JavaScript | Node.js and the package manager (`npm`, `pnpm` or `bun`, or the `package-manager` of the target) that installs npm dependencies; with the Wasm or JS target, they are errors when the project needs them, and it also checks a browser for a `browser` environment and Binaryen's `wasm-opt` |
| Network | The proxy (`[http] proxy` or `HTTPS_PROXY` and friends, passwords hidden), `[http] cainfo`, repository credentials whose environment variables are unset, and whether each repository (Maven Central outside a project) answers. `--offline` skips contacting them |
| Project | Each member's `kargo.toml` [warnings](../warnings/), the process that holds the build lock on the `target/` of the project or workspace (named once, with its command and process id), and `kargo.lock`: present, readable, and locking every declared dependency. A `kargo.toml` that does not load is reported here instead of stopping the command |

`kargo doctor` exits with 1 when a check is an error, and 0 otherwise, so a CI job can run it first.

| Option | Meaning |
|---|---|
| `--fix` | Make the safe fixes: remove the toolchains whose install was interrupted, so the next command that needs one installs it again |
| `--format <fmt>` | `text` (default) or `json`: `{ "version", "errors", "warnings", "checks": [{ "section", "name", "status", "message", "fix", "fixable" }], "fixed" }` |
| `--bug-report` | Print the report as a Markdown code block to paste into a bug report, with your home directory shortened to `~` |

## `kargo version`

Print the version of Kargo, like [`kargo --version`](#global-flags). It never reads a project. With `-v`, it also prints the release and the target the binary was built for. When a newer release exists, a note on stderr says so, at most once a day (see [`kargo self`](#kargo-self)).

```console
$ kargo version
kargo 0.1.4
$ kargo -v version
kargo 0.1.4
release: 0.1.4
host: x86_64-unknown-linux-gnu
```

## `kargo info`

Print project information: package, targets, frameworks, path dependencies, repositories and paths. Each framework shows its version; for Spring Boot, the Spring Framework, Spring Security and Jackson versions that `kargo.lock` resolved follow it, in parentheses (see [Spring Boot versions](../../guides/spring-boot/#versions)). A repository that a [mirror](../../guides/offline-and-mirrors/#repository-mirrors) or `--vendored` replaces shows where it is fetched from (`maven-central (via https://nexus.corp/central)`), and a `config:` line lists the [configuration files](../../guides/offline-and-mirrors/#configuration-files) Kargo read. At the root of a workspace, it prints the members, the default members, the Kotlin version, the lockfile, the target directory and the dependency graph between the members (`server -> core`); with `--format json`, each member lists its `dependencies`.

| Option | Default | Meaning |
|---|---|---|
| `--format <fmt>` | `text` | `text` or `json`. `json` also lists every dependency and repository, with the `effective-url` it is fetched from. See [Machine-readable output](../machine-readable-output/#kargo-info---format-json) |

## `kargo metadata`

Print a JSON description of the whole project for editors, language servers and other tools: every package with its source, test and resource roots (the output directories of `[[generate]]` steps included), the steps themselves, output directories, compiler arguments and plugins, entry points, tasks, and the resolved classpath (JVM) or klib path (Wasm, JS, Native) of every target and scope. Resolving may download dependencies and update `kargo.lock`, as `kargo tree` does.

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

A JVM target lists its `@Preview` composables in `previews`, and every target the `Res` class of `composeResources/` in `compose-resources`. A target lists the [source sets of several targets](../../guides/multiplatform/#source-sets-for-several-targets) it compiles in `source-sets` (`name`, `targets`, `refines` and the three roots), and their directories follow the common and the target's own in `source-roots`, `test-roots` and `test-fixture-roots`. The output has a `"version": 1` field and is described in [Machine-readable output](../machine-readable-output/#kargo-metadata). See [Editor integration](../../guides/editors/) for how editors use it.

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
| `--log-file <path>` | | Append a line for each message from the editor to this file, with how long each request took to answer. See [Status and logs](../../guides/editors/#status-and-logs) |

With check on save, saving a Kotlin or Java file runs `kargo check` in the background and adds its diagnostics for every target to the Kotlin server's; `checkTargets` limits the targets. It is on with Kargo's own server and needs `{ "checkOnSave": true }` in the editor's `initializationOptions` with the others. See [Check on save](../../guides/editors/#check-on-save).

Kargo's own Kotlin server downloads the sources jars of dependencies in the background, as `kargo fetch --sources` does, and loads them without a restart; `{ "fetchSources": false }` in `initializationOptions` turns that off. `{ "inlayHints": { "types": false } }` (also `parameterNames`, `implicitIt`, `implicitReceivers` and `operators`, which is off by default) turns off a kind of inlay hint (see [Inlay hints](../../guides/kotlin-server/#inlay-hints)). `{ "renameFiles": false }` keeps a rename of a class from renaming its file, and `{ "movePackageDirectories": false }` keeps a rename of a package from moving its directory (see [Rename](../../guides/kotlin-server/#rename)). `{ "status": true }` turns on the `kargo/status` notifications of indexing, resolving and checking (see [Status and logs](../../guides/editors/#status-and-logs)). See [Libraries](../../guides/kotlin-server/#libraries).

Editors that pass `--stdio` may keep it; Kargo always uses stdio. The exit code is 0 after `shutdown` and `exit`, 1 if the editor exits without `shutdown`. See [Editor integration](../../guides/editors/#language-server) for editor setups, the files it writes, the [code actions](../../guides/editors/#code-actions-in-kargotoml), the `kargo/runnables` request and the [`kargo/testTree`](../../guides/editors/#test-tree) request.

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
