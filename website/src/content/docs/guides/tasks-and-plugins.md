---
title: Tasks and plugins
description: Automate chores with tasks and hooks written as Kotlin scripts or shell commands, add compiler plugins, and write your own kargo subcommands.
---

Kargo has four extension points:

| Mechanism | Use it to |
|---|---|
| **Tasks** | Name Kotlin scripts or shell commands, like npm scripts or `make` targets |
| **Hooks** | Run a task or program automatically before or after builds, tests and packaging |
| **Compiler plugins** | Add any Kotlin compiler plugin from Maven |
| **Subcommands** | Add new `kargo <name>` commands |

Write tasks and hooks as [Kotlin scripts](../scripts/). A script is the same language as the rest of your project, runs the same on Windows, macOS and Linux, can use any Maven library, and is easier to read and test than a shell script. A one-line command such as `protoc ...` is still best as a [shell command](#shell-commands), which stays fully supported.

## Tasks

A package's tasks start with its [environment variables](../environment/): the ones from `[env]` and the `.env` files, then the task's own `env`, which wins over them. `--env-file` picks other `.env` files for a task, as in `kargo task --env-file .env.staging deploy`.

```toml title="kargo.toml"
[tasks]
gen = { script = "scripts/gen.kts", description = "Generate code", hooks = ["pre-build"] }
notes = { script = "scripts/notes.kt", env = { CHANNEL = "releases" } }
proto = "protoc --kotlin_out=src-jvm proto/*.proto"
ci = { depends-on = ["gen", "proto"], run = "kargo test --locked" }
```

```sh
kargo task              # list tasks with their descriptions (or `kargo task --list`)
kargo task gen          # run one
kargo gen               # shorthand: any task name works as a subcommand
kargo gen --verbose     # extra arguments become the script's `args`
```

Built-in commands take precedence: a task named `build`, `test` or `fmt` runs only as `kargo task build`, since `kargo build` compiles your project and `kargo fmt` and `kargo lint` [format and lint your code](../formatting-and-linting/). Kargo [warns](../../reference/warnings/#tasks-hidden-by-built-in-commands) about such a task whenever you build, and `kargo task` and `kargo --help` mark it. Hooks don't care about the name, so a hidden task still runs as a hook.

| Key | Meaning |
|---|---|
| `script` | A Kotlin script to run, instead of `run`: a `.kts` script or a `.kt` file, relative to the project root |
| `run` | A shell command or a list of commands, run in order |
| `windows-run` | Replaces `run` on Windows; when it is not set, `run` is used everywhere |
| `shell` | The shell to run the commands in: `sh`, `bash`, `cmd`, `powershell` or `pwsh` (default: `sh` on Unix, `cmd` on Windows) |
| `description` | Shown by `kargo task`, `kargo --help` and [shell completions](../../getting-started/installation/#4-shell-completions-optional) |
| `depends-on` | Tasks to run first (cycles are rejected) |
| `env` | Extra environment variables |
| `cwd` | Working directory, relative to the project root |
| `hooks` | Lifecycle hooks that run this task automatically |

A task has a `script` or a `run`, not both: `windows-run` and `shell` go with `run`, and Kargo rejects a task that mixes them with `script`. A task with only `depends-on` runs its dependencies.

### Kotlin scripts

`script` points at a Kotlin file that [`kargo script`](../scripts/) runs. A `.kts` script declares its dependencies with `@file:` annotations; a `.kt` file has a `/* kargo ... */` front matter and a `fun main`. Both are accepted.

```kotlin title="scripts/gen.kts"
import java.io.File

val name = System.getenv("KARGO_PKG_NAME")
val version = System.getenv("KARGO_PKG_VERSION")
val out = File("generated/Version.kt")
out.parentFile.mkdirs()
out.writeText("package com.example\n\nconst val VERSION = \"$name $version\"\n")
println("wrote ${out.path}")
```

```kotlin title="scripts/notes.kt"
/* kargo
*/

fun main(args: Array<String>) {
    val channel = System.getenv("CHANNEL")
    println("${System.getenv("KARGO_PKG_NAME")} ${System.getenv("KARGO_PKG_VERSION")} -> $channel ${args.joinToString()}")
}
```

What a script gets:

- **Its file.** `script` is relative to the project root, like `cwd`; for a task in `[workspace.tasks]` it is relative to the workspace root, and a workspace task that a member inherits keeps running the workspace's file. A file that does not exist fails the task with a message that names it.
- **Its environment.** The variables of a shell command: `KARGO_TASK`, `KARGO_HOOK` and the `KARGO_*` variables [listed below](#hook-variables) when the task runs as a hook, the project's `[env]` and `.env` files, and the task's own `env`.
- **Its arguments.** Anything after the task name in `kargo task <name> <args>` or `kargo <name> <args>` is the script's `args`. Only the task you ran gets them, not the tasks it depends on.
- **Its directory.** The project root, or the task's `cwd`.
- **Its dependencies and JDK.** From its own header, as for any [Kotlin script](../scripts/#declare-what-the-script-needs): it does not read the project's `[dependencies]` or `[toolchain]`. Add `@file:Jdk("21")` when it needs a particular JDK. The dependencies resolve with Kargo's resolver, so `--offline` and `[mirrors]` apply.
- **A cache.** Kargo compiles the script on its first run and starts the compiled classes after that, so a `pre-build` task costs a JVM startup, not a compile, until the script changes. See [How runs are cached](../scripts/#how-runs-are-cached).

A script that exits with a non-zero code, or throws, fails the task, and the build when the task runs as a hook; `kargo` then exits with `101`. A script can call `kargo` on the same project, even from a hook: see [the build lock](#hooks).

### Shell commands

`run` is a shell command or a list of commands, run in order. They run with `sh -c` on Unix and `cmd /C` on Windows, unless `shell` says otherwise, and receive `KARGO_TASK` plus the `KARGO_*` variables listed below. Arguments after the task name are appended to the last command.

```toml title="kargo.toml"
[tasks]
proto = "protoc --kotlin_out=src-jvm proto/*.proto"
gen = { run = ["mkdir -p build", "./gen.sh"], shell = "sh", description = "Generate code", hooks = ["pre-build"] }
site = { run = "mkdocs build", cwd = "website", env = { SITE_URL = "https://example.com" } }
```

```sh
kargo proto --verbose   # extra arguments are appended to the task's last command
kargo task fmt          # run one
kargo fmt --dry-run
```

#### Windows

Kotlin scripts run the same on every platform, so this only concerns shell tasks. By default they run in `cmd` on Windows, so Unix syntax such as `$KARGO_PKG_NAME`, `mkdir -p` or `./gen.sh` doesn't work there: `cmd` expands `%KARGO_PKG_NAME%` instead. The same goes for the variables from `[env]` and `.env` files: they are in the task's environment, but each shell reads them in its own syntax.

| Shell | Reads the variable `GREETING` as |
|---|---|
| `sh`, `bash` | `$GREETING` |
| `cmd` (the default on Windows) | `%GREETING%` |
| `powershell`, `pwsh` | `$env:GREETING` |

With `[env] GREETING = "hi"`, `echo $GREETING` in a `cmd` task prints `$GREETING`, because `cmd` doesn't expand it. Use `shell = "sh"` (needs the `sh` of Git for Windows or another one on `PATH`) or a `windows-run` with the Windows version of the commands, or write the task as a Kotlin script.

**Use one shell on every platform.** Set `shell = "sh"` and write POSIX commands once:

```toml title="kargo.toml"
[tasks]
envs = { run = "echo $KARGO_PKG_NAME $KARGO_PROFILE", shell = "sh" }
gen = { run = ["mkdir -p build", "./gen.sh"], shell = "sh" }
```

On Windows Kargo uses the `sh` from [Git for Windows](https://gitforwindows.org/), even when only `git` is on your `PATH`, or any other `sh` on `PATH` (MSYS2, Cygwin). If none is found, the task fails and tells you so. `shell = "pwsh"` works the same way for PowerShell 7, which also runs on macOS and Linux.

With `pwsh` (PowerShell 7.3 or newer) a command that exits with a non-zero code stops the task, and that exit code becomes the task's, like with `sh` and `cmd`. Windows PowerShell 5.1 (`shell = "powershell"`) keeps running after a failing native command, so only cmdlet errors and the exit code of the script's last native command fail the task; use `pwsh`, or check `$LASTEXITCODE` after each command, when a step in the middle must stop the task.

Arguments after `--` in `kargo task <name> -- <args>` reach the last command as written. In `cmd` a `%` in them is escaped so `%NAME%` stays literal text.

**Give Windows its own commands.** When there is no shared shell, put the Windows version in `windows-run`:

```toml title="kargo.toml"
[tasks]
envs = { run = "echo $KARGO_PKG_NAME $KARGO_PROFILE", windows-run = "echo %KARGO_PKG_NAME% %KARGO_PROFILE%" }
gen = { run = ["mkdir -p build", "./gen.sh"], windows-run = ["if not exist build mkdir build", "gen.bat"] }
```

On Windows Kargo runs `windows-run` if it is set and `run` otherwise; on every other platform it always runs `run`. Commands that work in both shells, such as `protoc`, need only `run`.

### In a workspace

In a [workspace](../workspaces/#tasks-and-plugins), `[workspace.tasks]` in the workspace's `kargo.toml` are tasks that run at the workspace root. Members can inherit them, and `[workspace.plugins]` too:

```toml title="kargo.toml (the workspace)"
[workspace.tasks]
release-notes = { script = "tools/notes.kts" }
codegen = { script = "tools/codegen.kts", hooks = ["pre-build"] }

[workspace.plugins.notify]
script = "tools/notify.kts"
hooks = ["post-build"]
```

```toml title="apps/server/kargo.toml"
[tasks]
codegen.workspace = true     # runs in apps/server before each of its builds

[plugins]
notify.workspace = true
```

At the workspace root, `kargo <name>` runs the workspace's task, or else the task of every member that defines one, in dependency order. Inside a member it runs the member's task, or else the workspace's. Tasks and hooks of a member also get `KARGO_WORKSPACE_DIR`, the workspace root.

Code that a tool generates from other files (protobuf classes, an OpenAPI client, a `BuildConfig`) is better declared with a [`[[generate]]` step](../code-generation/) than with a `pre-build` task. A step writes outside `src/`, skips itself while its inputs are unchanged, and tells the editors where its output is.

## Hooks

Hooks are lifecycle points: `pre-build`, `post-build`, `pre-test`, `post-test`, `pre-run` and `post-package`. `kargo check` runs `pre-build` hooks too, so generated sources get checked, but not `post-build`. Attach tasks with `hooks = [...]` (see above), or attach a Kotlin script or any executable as a **hook plugin**. A script is the recommended way:

```toml title="kargo.toml"
[plugins.notify]
script = "scripts/notify.kts"     # relative to the project root
args = ["--quiet"]                # the hook name is appended as the last argument
hooks = ["post-build", "post-test"]
env = { CHANNEL = "builds" }
```

```kotlin title="scripts/notify.kts"
@file:DependsOn("com.squareup.okhttp3:okhttp:4.12.0")

val hook = args.last()                          // "post-build"
val name = System.getenv("KARGO_PKG_NAME")
println("$name finished $hook")
```

A failing hook fails the build. A plugin script is run by [`kargo script`](../scripts/) like a task's: it gets the hook's environment (listed below) and the plugin's own `env`, runs in the project root, and is compiled on its first run and cached after that. `args` come first, and the hook name is always the last argument, so `args = ["--quiet"]` gives a script `["--quiet", "post-build"]`.

| Key | Meaning |
|---|---|
| `script` | A Kotlin script to run, instead of `command`: a `.kts` script or a `.kt` file, relative to the project root |
| `command` | The executable to run |
| `windows-command` | Replaces `command` on Windows; when it is not set, `command` is used everywhere |
| `args` | Arguments passed before the hook name |
| `windows-args` | Replaces `args` on Windows; when it is not set, `args` is used everywhere |
| `hooks` | Lifecycle hooks that run the plugin |
| `env` | Extra environment variables |

A plugin has a `script` or a `command`, not both. `windows-command` goes with `command`; a script runs the same on every platform and needs neither it nor a `.bat` twin.

### Executables

A hook plugin can also run any program with `command`:

```toml title="kargo.toml"
[plugins.notify]
command = "./scripts/notify.sh"   # relative to the project; bare names are looked up on PATH
args = ["--quiet"]                # the hook name is appended as the last argument
hooks = ["post-build", "post-test"]
env = { CHANNEL = "builds" }
```

`command` is started directly, not through a shell, so it is a program name (or a path) and never a command line. Put the arguments in `args`: `command = "cmd"` with `args = ["/c", "exit", "7"]` works, `command = "cmd /c exit 7"` does not, and Kargo says so when it cannot start a `command` that contains spaces. This differs from `run` of a task and `[[generate]]` step, which is a shell string.

| | `command` of a plugin | `run` of a task or generate step |
|---|---|---|
| Started | Directly, as a program | Through a shell (`sh`, or `cmd` on Windows; see `shell`) |
| Arguments | In `args` | Written in the string |
| Pipes, `&&`, `$VAR` | Not available | Available |

On Windows a `command` ending in `.sh` runs through `sh` (from Git for Windows or on `PATH`), so the example above works on every platform. Other commands must be real executables there (`.exe`, `.bat` or `.cmd`).

To run something else on Windows, set `windows-command` (and `windows-args` if the arguments differ):

```toml title="kargo.toml"
[plugins.notify]
command = "./scripts/notify.sh"
windows-command = "./scripts/notify.bat"
hooks = ["post-build"]
```

### Hook variables

Hooks receive these environment variables, whether they are tasks, scripts or executables:

| Variable | Available |
|---|---|
| `KARGO_HOOK` | always (e.g. `post-build`) |
| `KARGO_MANIFEST_DIR`, `KARGO_PKG_NAME`, `KARGO_PKG_VERSION`, `KARGO_PKG_GROUP` | always |
| `KARGO_WORKSPACE_DIR` | in a workspace: the workspace root |
| `KARGO_PROFILE`, `KARGO_TARGET_DIR`, `KARGO_KOTLIN_VERSION` | always |
| `KARGO_CLASSPATH`, `KARGO_JVM_JAR` | after a JVM build |
| `KARGO_PACKAGE` | after `kargo package`: the fat jar, or a library's jar; with `-t android`, the first App Bundle (or AAR for a library) |
| `KARGO_TEST_RESULT` | after tests |
| `KARGO_BUILD_LOCK` | always; see below |

Kargo holds the [build-directory lock](../../reference/commands/#concurrent-commands) while hooks run. A hook can still call `kargo` on the same project, because `KARGO_BUILD_LOCK` tells the nested command that its parent already holds the lock. This holds for a Kotlin script too: it runs with the variable, so a `ProcessBuilder(System.getenv("KARGO"), "metadata")` in a script does not wait on the build that started it.

### Example: copy the jar after packaging

As a Kotlin script, which needs no `windows-command`:

```kotlin title="scripts/publish.kts"
import java.io.File

val jar = File(System.getenv("KARGO_PACKAGE") ?: error("run this hook after `kargo package`"))
val dist = File("dist").apply { mkdirs() }
val target = File(dist, System.getenv("KARGO_PKG_NAME") + ".jar")
jar.copyTo(target, overwrite = true)
println("copied to ${target.path}")
```

```toml
[plugins.publish]
script = "scripts/publish.kts"
hooks = ["post-package"]
```

As shell scripts, one for each family of platforms:

```sh title="scripts/publish.sh"
#!/bin/sh
set -e
mkdir -p dist
cp "$KARGO_PACKAGE" "dist/$KARGO_PKG_NAME.jar"
echo "copied to dist/$KARGO_PKG_NAME.jar"
```

```bat title="scripts\publish.bat"
@echo off
if not exist dist mkdir dist
copy /Y "%KARGO_PACKAGE%" "dist\%KARGO_PKG_NAME%.jar" || exit /b 1
echo copied to dist\%KARGO_PKG_NAME%.jar
```

```toml
[plugins.publish]
command = "./scripts/publish.sh"
windows-command = "./scripts/publish.bat"
hooks = ["post-package"]
```

The hook variables are ordinary environment variables: `%KARGO_PACKAGE%` in `cmd`, `$env:KARGO_PACKAGE` in PowerShell.

## Compiler plugins

Any Kotlin compiler plugin published to Maven can be used. Give its coordinates, its plugin ID and any options:

```toml title="kargo.toml"
[plugins.power-assert]
maven = "org.jetbrains.kotlin:kotlin-power-assert-compiler-plugin-embeddable:2.4.21"
id = "org.jetbrains.kotlin.powerassert"
options = { function = "kotlin.assert" }
```

More examples:

```toml
# Make classes with these annotations open (like the Spring preset)
[plugins.allopen]
maven = "org.jetbrains.kotlin:kotlin-allopen-compiler-plugin-embeddable:2.4.21"
id = "org.jetbrains.kotlin.allopen"
options = { annotation = "com.example.Open" }

# Generate no-arg constructors
[plugins.noarg]
maven = "org.jetbrains.kotlin:kotlin-noarg-compiler-plugin-embeddable:2.4.21"
id = "org.jetbrains.kotlin.noarg"
options = { annotation = "com.example.NoArg" }
```

:::tip
Plugins from the Kotlin repository should use the same version as `[kotlin] version`.
:::

List everything that's configured:

```sh
kargo plugins
```

## Custom subcommands

Like Cargo, any executable named `kargo-<name>` in `~/.kargo/bin` or on your `PATH` becomes `kargo <name>`:

```sh title="~/.kargo/bin/kargo-loc"
#!/bin/sh
# Count lines of Kotlin in the project
find src* tests* -name '*.kt' | xargs wc -l | tail -1
```

```console
$ chmod +x ~/.kargo/bin/kargo-loc
$ kargo loc
     412 total
```

On Windows the file needs an extension: `kargo-<name>.exe`, `.bat` or `.cmd`, in `%USERPROFILE%\.kargo\bin` or on your `PATH`. There is no `chmod`; a batch file is enough:

```bat title="%USERPROFILE%\.kargo\bin\kargo-loc.bat"
@echo off
rem Count lines of Kotlin in the project
powershell -NoProfile -Command "(Get-ChildItem -Recurse -Include *.kt src*,tests* | Get-Content | Measure-Object -Line).Lines"
```

```console
> kargo loc
412
```

If a task and a subcommand share a name, the task wins.
