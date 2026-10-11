---
title: Tasks and plugins
description: Automate chores with tasks and hooks, add compiler plugins, and write your own kargo subcommands.
---

Kargo has four extension points:

| Mechanism | Use it to |
|---|---|
| **Tasks** | Name shell commands, like npm scripts or `make` targets |
| **Hooks** | Run a task or program automatically before or after builds, tests and packaging |
| **Compiler plugins** | Add any Kotlin compiler plugin from Maven |
| **Subcommands** | Add new `kargo <name>` commands |

## Tasks

```toml title="kargo.toml"
[tasks]
fmt = "ktlint -F \"src/**/*.kt\""
gen = { run = ["mkdir -p build", "./gen.sh"], description = "Generate code", hooks = ["pre-build"] }
site = { run = "mkdocs build", cwd = "website", env = { SITE_URL = "https://example.com" } }
ci = { depends-on = ["fmt", "gen"], run = "kargo test --locked" }
```

```sh
kargo task            # list tasks with their descriptions
kargo task fmt        # run one
kargo fmt             # shorthand: any task name works as a subcommand
kargo fmt --dry-run   # extra arguments are appended to the task's last command
```

| Key | Meaning |
|---|---|
| `run` | A command or a list of commands, run in order |
| `windows-run` | Replaces `run` on Windows; when it is not set, `run` is used everywhere |
| `shell` | The shell to run the commands in: `sh`, `bash`, `cmd`, `powershell` or `pwsh` (default: `sh` on Unix, `cmd` on Windows) |
| `description` | Shown by `kargo task` |
| `depends-on` | Tasks to run first (cycles are rejected) |
| `env` | Extra environment variables |
| `cwd` | Working directory, relative to the project root |
| `hooks` | Lifecycle hooks that run this task automatically |

Commands run with `sh -c` on Unix and `cmd /C` on Windows. They receive `KARGO_TASK` plus the `KARGO_*` variables listed below.

### Windows

By default tasks run in `cmd` on Windows, so Unix syntax such as `$KARGO_PKG_NAME`, `mkdir -p` or `./gen.sh` doesn't work there: `cmd` expands `%KARGO_PKG_NAME%` instead. You have two ways to write tasks that work everywhere.

**Use one shell on every platform.** Set `shell = "sh"` and write POSIX commands once:

```toml title="kargo.toml"
[tasks]
envs = { run = "echo $KARGO_PKG_NAME $KARGO_PROFILE", shell = "sh" }
gen = { run = ["mkdir -p build", "./gen.sh"], shell = "sh" }
```

On Windows Kargo uses the `sh` from [Git for Windows](https://gitforwindows.org/), even when only `git` is on your `PATH`, or any other `sh` on `PATH` (MSYS2, Cygwin). If none is found, the task fails and tells you so. `shell = "pwsh"` works the same way for PowerShell 7, which also runs on macOS and Linux.

**Give Windows its own commands.** When there is no shared shell, put the Windows version in `windows-run`:

```toml title="kargo.toml"
[tasks]
envs = { run = "echo $KARGO_PKG_NAME $KARGO_PROFILE", windows-run = "echo %KARGO_PKG_NAME% %KARGO_PROFILE%" }
gen = { run = ["mkdir -p build", "./gen.sh"], windows-run = ["if not exist build mkdir build", "gen.bat"] }
```

On Windows Kargo runs `windows-run` if it is set and `run` otherwise; on every other platform it always runs `run`. Commands that work in both shells, such as `ktlint -F`, need only `run`.

## Hooks

Hooks are lifecycle points: `pre-build`, `post-build`, `pre-test`, `post-test`, `pre-run` and `post-package`. `kargo check` runs `pre-build` hooks too, so generated sources get checked, but not `post-build`. Attach tasks with `hooks = [...]` (see above), or attach any executable as a **hook plugin**:

```toml title="kargo.toml"
[plugins.notify]
command = "./scripts/notify.sh"   # relative to the project; bare names are looked up on PATH
args = ["--quiet"]                # the hook name is appended as the last argument
hooks = ["post-build", "post-test"]
env = { CHANNEL = "builds" }
```

A failing hook fails the build.

| Key | Meaning |
|---|---|
| `command` | The executable to run |
| `windows-command` | Replaces `command` on Windows; when it is not set, `command` is used everywhere |
| `args` | Arguments passed before the hook name |
| `windows-args` | Replaces `args` on Windows; when it is not set, `args` is used everywhere |
| `hooks` | Lifecycle hooks that run the plugin |
| `env` | Extra environment variables |

`command` is started directly, not through a shell. On Windows a `command` ending in `.sh` runs through `sh` (from Git for Windows or on `PATH`), so the example above works on every platform. Other commands must be real executables there (`.exe`, `.bat` or `.cmd`).

To run something else on Windows, set `windows-command` (and `windows-args` if the arguments differ):

```toml title="kargo.toml"
[plugins.notify]
command = "./scripts/notify.sh"
windows-command = "./scripts/notify.bat"
hooks = ["post-build"]
```

Hooks receive these environment variables:

| Variable | Available |
|---|---|
| `KARGO_HOOK` | always (e.g. `post-build`) |
| `KARGO_MANIFEST_DIR`, `KARGO_PKG_NAME`, `KARGO_PKG_VERSION`, `KARGO_PKG_GROUP` | always |
| `KARGO_PROFILE`, `KARGO_TARGET_DIR`, `KARGO_KOTLIN_VERSION` | always |
| `KARGO_CLASSPATH`, `KARGO_JVM_JAR` | after a JVM build |
| `KARGO_PACKAGE` | after `kargo package` |
| `KARGO_TEST_RESULT` | after tests |

### Example: copy the jar after packaging

```sh title="scripts/publish.sh"
#!/bin/sh
set -e
mkdir -p dist
cp "$KARGO_PACKAGE" "dist/$KARGO_PKG_NAME.jar"
echo "copied to dist/$KARGO_PKG_NAME.jar"
```

```toml
[plugins.publish]
command = "./scripts/publish.sh"
hooks = ["post-package"]
```

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

If a task and a subcommand share a name, the task wins.
