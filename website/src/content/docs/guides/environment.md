---
title: Environment variables
description: Give your program its environment with [env] in kargo.toml and .env files per profile, mark variables as required, compile some into a BuildConfig object, and check the result with kargo env.
---

Server apps such as [Ktor](../ktor/) and [Spring Boot](../spring-boot/) read their configuration from environment variables. Kargo starts `kargo run` and `kargo test` programs with a **project environment**, built from three places: the `[env]` table in `kargo.toml`, `.env` files next to it, and your shell. Tasks and [hook plugins](../tasks-and-plugins/) get the same variables.

## Set variables in kargo.toml

A variable is a string, a number or a boolean. Use the table form when you need more than a value:

```toml title="kargo.toml"
[env]
APP_NAME = "demo"
PORT = 8080
LOG_LEVEL = { value = "debug", force = false }   # the default: a value already set in the shell wins
CONFIG_DIR = { value = "config", relative = true } # resolved against the package root
DATABASE_URL = { required = true }                 # no default: it must come from the shell or a .env file

[profile.release.env]                              # replaces [env] for release builds, variable by variable
LOG_LEVEL = "info"
```

| Key | Meaning |
|---|---|
| `value` | The value. Required unless `required = true` |
| `force` | `true` makes `value` win over the shell and the `.env` files too |
| `relative` | `true` resolves `value` against the package's directory, so `config` becomes an absolute path |
| `required` | `true` fails `kargo run`, `kargo test` and `kargo task` before anything runs when neither the shell nor a `.env` file sets the variable. Can't be combined with `value` |

`env-files` and `expose` are not variables; they are described below.

## Use .env files

Put settings that differ between machines or profiles in `.env` files in the package's directory:

| File | Read by | Priority |
|---|---|---|
| `.env` | every profile | lowest of the files |
| `.env.dev`, `.env.release`, `.env.<name>` for a custom profile | that profile only | above `.env` |
| `.env.local` | every profile | above the profile files |
| `.env.dev.local`, `.env.release.local`, `.env.<name>.local` | that profile only | highest of the standard files |

A missing file is skipped. `kargo new` adds `.env.local` and `.env.*.local` to `.gitignore`, so machine-specific values and secrets stay out of the repository. Commit a `.env` with safe defaults if your team wants one.

```sh title=".env"
# Defaults shared by every profile
APP_NAME=demo-app
DATABASE_URL=postgres://localhost/dev
API_TOKEN="s3cret"
```

```sh title=".env.local"
# This machine only, ignored by git
API_TOKEN=local-token
```

The format is the usual dotenv:

- Lines are `KEY=value`. An `export ` prefix, empty values and `#` comments on their own lines are fine. An unquoted value ends at a ` #` comment.
- Single quotes take the value literally. Double quotes understand `\n`, `\t`, `\"` and `\\`, and can span several lines.
- `${NAME}` and `${NAME:-default}` insert another variable: a key set earlier in the same file or in a file read before it, otherwise the shell's value. Variables from kargo.toml are not used. An unset name becomes the default, or nothing. Single-quoted values are not expanded.

```sh title=".env.dev"
BASE_URL=http://localhost:${PORT:-8080}
LOG_FILE=${LOG_DIR:-logs}/app.log
```

### More files with env-files

`env-files` in `[env]` names more `.env` files, relative to the package root. They are read after the standard ones, so they win over them. A profile can add its own list in `[profile.<name>.env]`, read after the `[env]` list and only for that profile:

```toml title="kargo.toml"
[env]
env-files = ["config/shared.env"]

[profile.release.env]
env-files = ["config/release.env"]
```

A file named in `env-files` must exist; a missing one is an error that names the key. `--no-env-file` skips these files too.

## Precedence

From the highest priority to the lowest, the first source that sets a variable wins:

1. A variable in kargo.toml with `force = true`
2. The shell's environment
3. `--env-file` files, the last one given first
4. `.env.<profile>.local`
5. `.env.local`
6. `.env.<profile>`
7. `.env`
8. `env-files` of `[profile.<name>.env]`, the last one first
9. `env-files` of `[env]`, the last one first
10. `[profile.<name>.env]`
11. `[env]`

Without `force`, a variable you export in your shell beats everything in the project, so you can override any value for one run:

```sh
$ LOG_LEVEL=trace kargo run
```

## Required variables

`required = true` says the program can't work without the variable. `kargo run`, `kargo test` and `kargo task` check for it before they build or run anything:

```console
$ kargo run
error: required environment variable not set: `DATABASE_URL` (declared in [env])
set it in your shell or in a .env file, or give it a `value` in kargo.toml
```

Builds don't check. `kargo build`, `kargo check`, `kargo package` and `kargo publish` go ahead without it, and the build hooks they run get only the variables that are set. Android and Wasm browser programs don't get the environment at all, so they don't need the check either.

## Check the result with kargo env

`kargo env` lists every variable the program gets, the source that won, and the value. Values are hidden by default because they often hold secrets:

```console
$ kargo env
API_TOKEN     .env.local:1  ********
APP_NAME      .env:1        ********
CONFIG_DIR    [env]         ********
DATABASE_URL  .env.dev:1    ********
LOG_LEVEL     [env]         ********
note: values are hidden; pass --show-values to print them
```

`--show-values` prints them, and `--release` (or `--profile <name>`) shows that profile's variables instead of the dev ones. The source is `shell`, a section of kargo.toml such as `[env]` or `[profile.release.env]`, or a `.env` file with its line number. A required variable that nothing sets is listed as `missing (required)`. `kargo env` itself succeeds, so you can look at a machine that isn't ready yet.

### Checks for CI

`kargo env --check` is the check for a CI job or a new machine. It fails and lists every problem when:

- a required variable is not set, and
- a key of `.env.example` is neither set in the shell nor declared by any source.

```sh title=".env.example"
# Every variable a developer must set; values are examples only
DATABASE_URL=
API_TOKEN=
```

```console
$ kargo env --check
error: the environment is incomplete:
  `DATABASE_URL` is required but not set (declared in [env])
  `API_TOKEN` is listed in .env.example but not set
```

Without a `.env.example`, `--check` looks at the required variables only.

### Secrets in git

Kargo warns when a `.env` file that git tracks sets a variable whose name looks like a secret (it contains `SECRET`, `TOKEN`, `PASSWORD`, `PASSWD` or `CREDENTIAL`, or it is `KEY` or ends in `_KEY`) with a value. The warning is printed by `kargo run`, `kargo test`, `kargo task` and `kargo env`, and it is listed in the [warnings reference](../../reference/warnings/#env-file-tracked-by-git). Move the secret into a `.local` file, which is git-ignored, or into your shell.

## Choose the files for one command

`--env-file <path>` reads one more `.env` file after the standard ones, so its values win over them. Repeat it for several files. `--no-env-file` reads none of the `.env` files; the variables in kargo.toml and the shell still apply. The two flags can't be combined.

```sh
$ kargo run --env-file .env.staging
$ kargo test --no-env-file
$ kargo env --env-file .env.staging --show-values
```

Tasks take the same flags, either before the task name or right after it:

```sh
$ kargo task --env-file .env.staging deploy
$ kargo deploy --env-file=.env.staging
```

Flags after the task name are taken only from the front of its arguments, so `kargo deploy --env-file .env.staging --dry-run` passes `--dry-run` on to the task. Workspace tasks run at the root, which has no `[env]` of its own, so they get no project environment.

Build commands don't take these flags. Their hooks read the standard files.

## Compile variables into the code

Some values belong in the program itself, such as a version or a build identifier. List them in `expose` in `[env]`, and Kargo generates a Kotlin `object` with one property for each:

```toml title="kargo.toml"
[env]
expose = ["APP_NAME", "BUILD_ID"]
APP_NAME = "demo"
```

The object is `BuildConfig` in a package made from the group and name of the package (`my-app` becomes `my_app`; with `group = "com.example"`, it is `com.example.my_app`):

```kotlin title="src/Main.kt"
import my_app.BuildConfig

fun main() {
    println("${BuildConfig.APP_NAME}")
}
```

A variable that is set becomes a `const val` with its value, and one that is not set becomes a nullable `val` with `null`, so a build doesn't fail for a variable only some machines have. The file is written to `target/<profile>/<target>/generated/env/BuildConfig.kt`, and it is rewritten only when its contents change.

`expose` applies to the JVM, Native and Wasm (Node.js) targets. It is not generated for Android, which has its own `build-config` in [`[target.android]`](../android/), nor for Wasm browser apps.

## Where the variables go

| Program | Gets the project environment | Required variables checked |
|---|---|---|
| `kargo run` on the JVM, Native, and Wasm with `node` | Yes | Yes |
| `kargo test` on the JVM, Native, and Wasm tests on Node.js | Yes | Yes |
| `kargo task` and `kargo <task>` (package tasks) | Yes; a task's own `env` still wins | Yes |
| Build hooks and [hook plugins](../tasks-and-plugins/#hooks) | Yes, the variables that are set | No |
| Wasm browser apps and browser tests | No: the browser doesn't read process variables | No |
| Android (`kargo run -t android`, `kargo test -t android`) | No: Gradle installs the app, which gets no process variables | No |

The variables are not written into your build output, except those you list in `expose`. Read the others at run time with `System.getenv` (JVM), `platform.posix.getenv` (Native) or `process.env` (Node.js).

Only the variables themselves are listed in [`kargo.toml`](../../reference/manifest/#env), and `kargo env` is in the [command reference](../../reference/commands/#kargo-env).
