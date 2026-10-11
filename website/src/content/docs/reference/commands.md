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
| `--refresh` | Ignore cached metadata and `kargo.lock`, and re-check repositories |
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

The generated code lives in the package `<group>.<name>`, with the name lowercased and every character other than a letter or digit replaced by `_` (`my-app` becomes `com.example.my_app`). A segment that is a Kotlin or Java keyword gets a `_` suffix, so `kargo new class` uses `com.example.class_`.

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
| `filter` | Only run tests whose class name (JVM) or test name (Native) contains this string. Ignored on Wasm. |
| `--no-run` | Compile tests but don't run them |
| `-r, --release` | Use the release profile |
| `-t, --target <target>` | Only this target (repeatable) |

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

Without a version, the latest release is used.

## `kargo remove` (`rm`)

Remove dependencies from `kargo.toml`.

```sh
kargo remove <group:artifact or key>... [--dev] [-t <target>]
```

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

Re-resolve all dependencies and rewrite `kargo.lock`.

## `kargo clean`

Delete `target/`.

| Option | Meaning |
|---|---|
| `-r, --release` | Only delete the release profile's output |

## `kargo task`

List tasks, or run one. Extra arguments are appended to the task's last command.

```sh
kargo task               # list
kargo task <name> [args]
kargo <name> [args]      # shorthand
```

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

## `kargo info`

Print project information: package, targets, toolchains and paths.

## `kargo <name>`

If `<name>` is a task in `kargo.toml`, run it. Otherwise run the executable `kargo-<name>` from `~/.kargo/bin` or `PATH`.
