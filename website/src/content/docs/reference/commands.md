---
title: Commands
description: Every kargo command and flag.
---

Run `kargo help <command>` for the same information in your terminal.

## Global flags

These work with every command.

| Flag | Meaning |
|---|---|
| `-v`, `--verbose` | Show the compiler and tool commands Kargo runs |
| `-q`, `--quiet` | Don't print Kargo's status messages |
| `--offline` | Never use the network (also `KARGO_OFFLINE=1`) |
| `--locked` | Fail if `kargo.lock` would need to change |
| `--refresh` | Ignore cached metadata and `kargo.lock`, re-check repositories, and re-verify cached artifacts (re-downloading corrupt ones) |
| `--manifest-path <PATH>` | Use this `kargo.toml` instead of searching from the current directory |
| `-V`, `--version` | Print the version |
| `-h`, `--help` | Print help |

**Exit codes:** `0` on success, `101` when a build or test fails or Kargo reports an error (as in Cargo). `kargo run` exits with your program's exit code.

Targets are `jvm`, `wasm`, `native` and `android`.

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
| `-w, --with <list>` | | Frameworks: `compose`, `serialization`, `coroutines`, `ktor`, `lombok`, `spring-boot` |
| `--group <group>` | `com.example` | Maven group, also the base Kotlin package |
| `--name <name>` | directory name | Package name |
| `--kotlin <version>` | `2.4.21` | Kotlin version |
| `--no-vcs` | | Don't create a git repository |

```sh
kargo new hello
kargo new mylib --lib -t jvm,wasm,native
kargo new api -w ktor,serialization --group io.acme
```

The generated `kargo.toml` stays minimal: a `[package]` section, the `[target.*]` sections for any non-JVM targets (plus an empty `[target.jvm]` when JVM is one of several targets) and `[frameworks]` when you pass `--with`. It has no `[kotlin]` section unless `--kotlin` picks a non-default version, so the project uses Kargo's default Kotlin version and JVM target. Add `[dependencies]`, `[dev-dependencies]` and `[tasks]` when you need them; `kargo add` creates the dependency sections for you.

The generated code lives in the package `<group>.<name>`, with the name lowercased and every character other than a letter or digit replaced by `_` (`my-app` becomes `com.example.my_app`). A segment that is a Kotlin or Java keyword gets a `_` suffix, so `kargo new class` uses `com.example.class_`.

Kargo runs `git init` in the new directory when `git` is on the `PATH`, unless you pass `--no-vcs` or the directory is already inside a git repository (a monorepo or an `examples/` folder), in which case the project joins that repository instead of nesting a new one. A `.gitignore` is written either way, unless one already exists.

## `kargo init`

Same as `kargo new`, but in an existing directory (the current one by default). Existing files are not overwritten.

```sh
kargo init [path] [options]
```

## `kargo build` (`b`)

Compile all enabled targets, or the ones you name.

| Option | Meaning |
|---|---|
| `-r, --release` | Use the release profile |
| `-t, --target <target>` | Only this target (repeatable) |

```sh
kargo build
kargo build --release -t jvm -t native
```

## `kargo check` (`c`)

Type-check all enabled targets, or the ones you name, and report compiler errors and warnings without packaging or linking. It is the fastest feedback loop for editors and CI lint stages.

| Option | Meaning |
|---|---|
| `--tests` | Also check the test sources (`tests/`, `tests-<target>/`) |
| `-r, --release` | Use the release profile |
| `-t, --target <target>` | Only this target (repeatable) |

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
| `-t, --target <target>` | Target to run. Default: the first enabled of `jvm`, `native`, `wasm`, `android` |

How each target runs:

| Target | Runs |
|---|---|
| JVM | `java -cp ... <main-class>` with `jvm-args` |
| Wasm | `node target/<profile>/wasm/<name>.mjs` |
| Native | the executable built for the host platform |
| Android | installs on the connected device with Gradle, then launches it with `adb` |

## `kargo test` (`t`)

Build and run tests.

```sh
kargo test [filter] [options]
```

| Option | Meaning |
|---|---|
| `filter` | Only run tests whose `package.Class.method` name contains this string, for example `MathTest` or `MathTest.adds`. Ignored on Wasm. See [Filtering tests](../../guides/testing/#filtering-tests). |
| `--no-run` | Compile tests but don't run them |
| `-r, --release` | Use the release profile |
| `-t, --target <target>` | Only this target (repeatable) |

## `kargo doc` (`d`)

Generate HTML API documentation from KDoc comments with Dokka, into `target/debug/doc/`.

```sh
kargo doc [--open] [-t <target>]
```

| Option | Meaning |
|---|---|
| `--open` | Open the documentation in a browser when it is generated (the browser in `BROWSER`, else the system default) |
| `-t, --target <target>` | Only this target (repeatable); defaults to every enabled target |

Configure it with [`[doc]`](../manifest/#doc). See [API documentation](../../guides/documentation/).

## `kargo fmt`

Format the Kotlin files in `src/`, `src-<target>/`, `tests/` and `tests-<target>/` with ktlint.

```sh
kargo fmt [--check]
```

| Option | Meaning |
|---|---|
| `--check` | Report unformatted code without changing files; exit with code 101 if there is any |

Without `--check`, files are rewritten in place, and the command exits with code 101 if ktlint found problems it can't fix. ktlint reads `.editorconfig`; configure the rest with [`[fmt]`](../manifest/#fmt). See [Formatting and linting](../../guides/formatting-and-linting/).

## `kargo lint`

Analyze the Kotlin files in `src/`, `src-<target>/`, `tests/` and `tests-<target>/` with detekt. Exits with code 101 if detekt reports issues.

```sh
kargo lint [--create-baseline]
```

| Option | Meaning |
|---|---|
| `--create-baseline` | Record the current issues in the baseline file (`detekt-baseline.xml` by default) instead of reporting them |

detekt reads `detekt.yml`; configure the rest with [`[lint]`](../manifest/#lint). See [Formatting and linting](../../guides/formatting-and-linting/).

## `kargo add`

Add dependencies to `kargo.toml`, or enable frameworks.

```sh
kargo add <group:artifact[:version[:classifier]]>... [options]
kargo add -F <framework,...>
```

| Option | Meaning |
|---|---|
| `--dev` | Add to `[dev-dependencies]` |
| `-t, --target <target>` | Add to `[target.<target>.dependencies]` |
| `--scope <scope>` | `compile`, `runtime` or `provided` |
| `--bom` | Import as a BOM |
| `--pre` | Allow pre-release versions when picking the latest |
| `--no-verify` | Don't check that the artifact exists |
| `-F, --framework <list>` | Enable built-in frameworks instead |

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

Without an option, the dependency is removed from `[dependencies]`.

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

"Compatible" follows the same rule as the `compatible` column of [`kargo outdated`](#kargo-outdated). `kargo.lock` is re-resolved on the next build because the manifest changed.

## `kargo tree`

Show the resolved dependency graph. Conflicts are shown as `requested -> selected`; repeated subtrees are marked `(*)`.

| Option | Default | Meaning |
|---|---|---|
| `-t, --target <target>` | first enabled | Which target |
| `-c, --configuration <c>` | `runtime` | `compile`, `runtime` or `test` |
| `--depth <n>` | unlimited | Maximum depth |
| `-d, --duplicates` | | Only modules requested in several versions |

## `kargo package`

Build a self-contained, runnable fat jar with all runtime dependencies: `target/<profile>/jvm/<name>-<version>-all.jar`. JVM only.

| Option | Meaning |
|---|---|
| `-r, --release` | Use the release profile |

## `kargo fetch`

Download all dependencies and toolchains so later builds can run offline.

| Option | Meaning |
|---|---|
| `-t, --target <target>` | Only this target (repeatable) |

## `kargo update`

Re-resolve dependencies and rewrite `kargo.lock`. Cached repository metadata is ignored, so new versions within version ranges are picked up.

```sh
kargo update [group:artifact]... [--precise <version>] [--dry-run]
```

| Option | Meaning |
|---|---|
| `modules` | Only update these locked modules (direct or transitive). Every other module keeps its locked version |
| `--precise <version>` | Lock the one given module at exactly this version, for example to pick up a security fix in a transitive dependency or to step back from a broken release |
| `--dry-run` | Print the changes without writing `kargo.lock` |

Each change is printed as `Updating`, `Adding` or `Removing`:

```console
$ kargo update org.jetbrains.kotlinx:kotlinx-io-core --precise 0.7.0
    Updating org.jetbrains.kotlinx:kotlinx-io-core v0.6.0 -> v0.7.0
     Updated kargo.lock
```

`kargo update` changes only `kargo.lock`, never `kargo.toml`; use [`kargo upgrade`](#kargo-upgrade) to change the versions you declared. A `--precise` version stays locked until `kargo.toml` changes or you run `kargo update` for that module again. See [kargo.lock](../lockfile/#commands).

## `kargo clean`

Delete `target/`.

| Option | Meaning |
|---|---|
| `-r, --release` | Only delete the release profile's output |

## `kargo task`

List tasks, or run one. Extra arguments are appended to the task's last command.

```sh
kargo task               # list
kargo task --list        # the same
kargo task <name> [args]
kargo <name> [args]      # shorthand
```

Inside a project, `kargo --help` also lists its tasks.

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

Manage JDKs and Kotlin/Native distributions.

| Subcommand | Meaning |
|---|---|
| `list` (`ls`) | Installed toolchains, disk usage and what this project uses |
| `install [jdk\|native] [version]` | Install a toolchain; with no arguments, whatever the project needs |
| `remove` (`rm`) `<jdk\|native> <version>` | Remove one. For JDKs, a major version removes all its releases |
| `available` | JDK versions available for download (LTS marked) |
| `which` | The toolchains this project resolves to |

## `kargo daemon`

Show or stop the background compiler daemons that keep the Kotlin compiler warm between builds. See [Build performance](../../guides/build-performance/#the-compiler-daemon).

| Subcommand | Meaning |
|---|---|
| `status` | List running daemons with their process ID and state directory |
| `stop` | Stop every running daemon; the next build starts a new one |

## `kargo info`

Print project information: package, targets, toolchains and paths.

## `kargo completions`

Print a shell completion script for `bash`, `zsh`, `fish`, `powershell` or `elvish`. It completes commands, flags and targets, plus the current project's tasks, `kargo-*` plugins, framework names and installed toolchains. See [Shell completions](../../getting-started/installation/#4-shell-completions-optional) for how to load it.

```sh
kargo completions bash
```

## `kargo <name>`

If `<name>` is a task in `kargo.toml`, run it. Otherwise run the executable `kargo-<name>` from `~/.kargo/bin` or `PATH`.
