---
title: Environment variables
description: Give your program its environment with [env] in kargo.toml and .env files per profile, mark variables as required, and check the result with kargo env.
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
| `required` | `true` fails the command before anything builds when neither the shell nor a `.env` file sets the variable. Can't be combined with `value` |

## Use .env files

Put settings that differ between machines or profiles in `.env` files in the package's directory:

| File | Read by | Priority |
|---|---|---|
| `.env` | every profile | lowest of the files |
| `.env.dev`, `.env.release` | that profile only | above `.env` |
| `.env.local` | every profile | above the profile files |
| `.env.dev.local`, `.env.release.local` | that profile only | highest of the files |

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

## Precedence

From the highest priority to the lowest, the first source that sets a variable wins:

1. A variable in kargo.toml with `force = true`
2. The shell's environment
3. `--env-file` files, the last one given first
4. `.env.<profile>.local`
5. `.env.local`
6. `.env.<profile>`
7. `.env`
8. `[profile.<name>.env]`
9. `[env]`

Without `force`, a variable you export in your shell beats everything in the project, so you can override any value for one run:

```sh
$ LOG_LEVEL=trace kargo run
```

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

`--show-values` prints them, and `--release` shows the release profile's variables instead of the dev ones. The source is `shell`, a section of kargo.toml such as `[env]` or `[profile.release.env]`, or a `.env` file with its line number.

A required variable that nothing sets is listed as `missing (required)`, and the command fails:

```console
$ kargo env
CACHE_URL     [env]  missing (required)
DATABASE_URL  [env]  missing (required)
note: values are hidden; pass --show-values to print them
error: required environment variables not set: `CACHE_URL` (declared in [env]), `DATABASE_URL` (declared in [env])
set them in your shell or in a .env file, or give them a `value` in kargo.toml
```

Commands that start your program, run a task or run build hooks check first, so `kargo run`, `kargo test`, `kargo task` and the commands that build (`kargo build`, `kargo check`, `kargo package`, `kargo publish`) fail with the same error before they compile anything. Build hooks receive the variables too.

## Choose the files for one command

`--env-file <path>` reads one more `.env` file after the standard ones, so its values win over them. Repeat it for several files. `--no-env-file` reads none of the `.env` files; the variables in kargo.toml and the shell still apply. The two flags can't be combined.

```sh
$ kargo run --env-file .env.staging
$ kargo test --no-env-file
$ kargo env --env-file .env.staging --show-values
```

These flags work with `kargo run`, `kargo test` and `kargo env`. Build hooks and tasks always read the standard files.

## Where the variables go

| Program | Gets the project environment |
|---|---|
| `kargo run` on the JVM, Native, and Wasm with `node` | Yes |
| `kargo test` on the JVM, Native, and Wasm tests on Node.js | Yes |
| `kargo task` and `kargo <task>` | Yes (`[env]`, the `.env` files; a task's own `env` still wins) |
| Build hooks and [hook plugins](../tasks-and-plugins/#hooks) | Yes |
| Wasm browser apps and browser tests | No: the browser doesn't read process variables |
| Android (`kargo run -t android`) | No: Gradle installs the app, which gets no process variables |

The variables are not written into your build output. Read them at run time with `System.getenv` (JVM), `platform.posix.getenv` (Native) or `process.env` (Node.js).

:::note
Kargo doesn't check `.env` files for secrets it might have committed by mistake. Keep anything secret in a `.local` file or in your shell.
:::

Only the variables themselves are listed in [`kargo.toml`](../../reference/manifest/#env), and `kargo env` is in the [command reference](../../reference/commands/#kargo-env).
