---
title: Code generation
description: Generate Kotlin and Java sources and resources from other files before they are compiled, with up-to-date checks, a BuildConfig object and protobuf and gRPC built in.
---

Many projects compile code that nobody wrote by hand: classes from `.proto` files, an HTTP client from an OpenAPI description, constants for the version and the Git commit. Kargo runs the tools that write that code with `[[generate]]` steps in `kargo.toml`.

A step runs before the compilers, writes into its own directory under `target/`, and Kargo compiles that directory with your sources. Nothing is written into `src/`, so there is nothing to commit or to ignore.

```toml title="kargo.toml"
[[generate]]
name = "protos"
inputs = ["proto/**/*.proto"]
run = "protoc -Iproto --java_out=$KARGO_GEN_DIR proto/*.proto"
targets = ["jvm", "android"]
```

```console
$ kargo build
  Generating `protos`
   Compiling app v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.12s
$ kargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.05s
```

The second build neither runs `protoc` nor compiles: the files under `proto/` did not change. Change one, and the step runs again.

## Where the output goes

Each step gets a directory of its own, `target/generated/<profile>/<name>/`, and the variable `KARGO_GEN_DIR` holds its path. The step writes its files there, in directories that match the packages of the code if it likes. The directory is empty when the step starts.

What the directory is used for depends on `kind`:

| `kind` | The directory is compiled as |
|---|---|
| `"sources"` (default) | Kotlin and Java sources of the program, like `src/` |
| `"test-sources"` | Kotlin and Java sources of the tests, like `tests/` |
| `"resources"` | Resources on the classpath, like `resources/` (JVM and Android only) |

A tool that writes a whole project, with tests, documentation and build files next to the code, would have all of that compiled. Name the directories that hold the code with `roots` (relative to `$KARGO_GEN_DIR`), and only they are compiled:

```toml title="kargo.toml"
roots = ["src/main/kotlin"]
```

`targets` limits the step to some targets. Without it, the step runs for every target and each target compiles what it wrote. A step for `["jvm", "android"]` never runs for a Wasm or Native build, and its classes never end up in them. In a workspace, each member has its own `target/generated/<profile>/<member>/` directory.

## When a step runs

A step with `inputs` runs only when something it depends on changed since it last succeeded. Kargo hashes:

- the contents of the files the `inputs` globs match (gitignore syntax, relative to the project root; a directory stands for the files under it);
- the step's definition in `kargo.toml`: the command, the tool and its arguments, `env`, `targets` and `kind`;
- the package name, version and group, the profile and Kargo's own version.

If the hash is the same as last time and the output directory is still there, the step is skipped. Delete the directory, run `kargo generate --force`, or run `kargo clean` to make it run again.

A step without `inputs` cannot tell whether it is out of date, so it runs on every build. The [`build-config`](#buildconfig) built-in is one: it is cheap and rewrites its file only when the text changes.

A step that runs does not make the compilers do needless work. Kargo keeps the files of the last run, and when the step writes a file with the same bytes as before, the old file stays in place, with its old modification time. A step that rewrites a hundred files because one `.proto` changed recompiles only what really changed. A file the step no longer writes is gone, and with it the class compiled from it.

When a step fails, the files of the last good run are put back, and the next build runs the step again.

## Writing a command

`run` is a shell command, run at the project root. It takes everything a [task](../tasks-and-plugins/) takes: a list of commands, `windows-run` for Windows, and `shell` to choose the shell.

```toml title="kargo.toml"
[[generate]]
name = "messages"
inputs = ["messages/*.txt"]
run = 'for f in messages/*.txt; do n=$(basename "$f" .txt); echo "class $n" > "$KARGO_GEN_DIR/$n.kt"; done'
windows-run = 'for %f in (messages\*.txt) do @echo class %~nf> "%KARGO_GEN_DIR%\%~nf.kt"'
```

On Windows the default shell is `cmd`, which does not expand `$KARGO_GEN_DIR`. Write a `windows-run`, or set `shell = "sh"` (Git for Windows has one) and use the same command everywhere.

The step gets the variables of [hooks](../tasks-and-plugins/#hooks) (`KARGO_MANIFEST_DIR`, `KARGO_PKG_VERSION`, `KARGO_PROFILE`, ...), the project's [`[env]`](../environment/) variables, and:

| Variable | Value |
|---|---|
| `KARGO_GEN_DIR` | The output directory of the step |
| `KARGO_GEN_ROOT` | The directory that holds the output directories of all steps, to read what another step wrote |
| `KARGO_GEN_NAME` | The name of the step |
| `KARGO_GEN_KIND` | `sources`, `test-sources` or `resources` |

The step's own `env` table comes last and wins.

Kargo holds back the output of a step. A step that fails shows its name, its exit code and everything it printed; one that works shows nothing, unless you run `kargo build -v`, which also prints each command line.

```console
$ kargo build
  Generating `protos`
error: generator `protos` failed

Caused by:
  `protoc -Iproto --java_out=$KARGO_GEN_DIR proto/*.proto` failed (exit code 1)
proto/user.proto:7:3: "Strng" is not defined.
```

## Running a tool from Maven

`tool` runs the main class of a Maven artifact on the project's JDK, so the generator needs no global install:

```toml title="kargo.toml"
[dependencies]
"com.squareup.okhttp3:okhttp" = "4.12.0"
"com.squareup.moshi:moshi-kotlin" = "1.15.2"
"com.squareup.moshi:moshi-adapters" = "1.15.2"

[[generate]]
name = "api-client"
inputs = ["openapi.yaml"]
tool = "org.openapitools:openapi-generator-cli:7.16.0"
args = ["generate", "-g", "kotlin", "-i", "openapi.yaml", "-o", "$KARGO_GEN_DIR"]
roots = ["src/main/kotlin"]
```

The `kotlin` generator writes a whole Gradle project, so `roots` picks the directory with its code, and the libraries the client uses are dependencies like any other. The generated models are then at `org.openapitools.client.models`.

Kargo resolves the artifact and its dependencies like any other, pins them in [`kargo.lock`](../../reference/lockfile/) under `generate/<name>` (so `--locked`, `--offline`, mirrors and `kargo vendor` apply), and runs `java -cp <classpath> <main class> <args>`. The main class is the `Main-Class` of the jar's manifest; set `main-class` for a tool that has none.

No shell runs the arguments, so Kargo replaces `$NAME` and `${NAME}` in them with the step's variables itself. A name that is not a variable stays as written.

## Order and parallel steps

Steps without a dependency on each other run at the same time. `after` makes a step wait for others:

```toml title="kargo.toml"
[[generate]]
name = "schema"
inputs = ["api/schema.json"]
run = "./tools/to-yaml.sh api/schema.json $KARGO_GEN_DIR/api.yaml"

[[generate]]
name = "client"
inputs = ["api/schema.json"]
tool = "org.openapitools:openapi-generator-cli:7.16.0"
args = ["generate", "-g", "kotlin", "-i", "$KARGO_GEN_ROOT/schema/api.yaml", "-o", "$KARGO_GEN_DIR"]
roots = ["src/main/kotlin"]
after = ["schema"]
```

`schema` finishes before `client` starts, and `client` reads the file it wrote. A step named in `after` runs even when the build is for targets it does not list. A cycle in `after` is an error when `kargo.toml` loads.

## Run steps on their own

`kargo build`, `check`, `test`, `run` and `package` run the steps they need first, once per command, also for a [path dependency](../workspaces/) that is built along. To run them without building, use `kargo generate`:

```sh
kargo generate                  # every step
kargo generate protos           # one step, and the steps it waits for
kargo generate -t jvm           # the steps for the JVM target
kargo generate --force          # even steps that are up to date
kargo generate --release        # the release profile's output
```

It is the command for CI checks that the generators still work, and for an editor that wants the generated files before the first build. `kargo clean` deletes `target/generated/` with the rest of `target/`, and `kargo clean --release` the release profile's.

Steps run in the build, so [`kargo watch`](../watch/) and `--watch` run them again when a file their `inputs` match changes.

Other commands use what steps wrote as well. `kargo doc` documents the generated code with the rest, `kargo package` and `kargo publish` put it in the library's sources jar, and KSP processors see it next to the code in `src/`.

## Built-in generators

Common cases need no command. Set `builtin` instead of `run` or `tool`.

### BuildConfig

`builtin = "build-config"` writes a Kotlin `object` with facts about the build:

```toml title="kargo.toml"
[[generate]]
name = "config"
builtin = "build-config"
package = "com.example.app"
fields = { API_URL = "https://api.example.com", RETRIES = 3, DEBUG = false }
```

```kotlin title="src/Main.kt"
import com.example.app.BuildConfig

fun main() {
    println("${BuildConfig.NAME} ${BuildConfig.VERSION} (${BuildConfig.GIT_SHA}) ${BuildConfig.PROFILE}")
}
```

| Constant | Value |
|---|---|
| `VERSION`, `GROUP`, `NAME` | `[package]` `version`, `group` (empty without one) and `name` |
| `GIT_SHA` | The commit `HEAD` points to, or `"unknown"` without Git or a commit |
| `PROFILE` | `dev`, `release` or the custom profile's name |
| `BUILD_TIME` | Only with `build-time = true`: an RFC 3339 time in UTC (see below) |
| your `fields` | A string, integer (`Int`, or `Long` when it is large) or boolean, as a `const val` |
| your `expose` | A variable of the project [environment](../environment/): a `const val` with its value, or a nullable `val` with `null` when it is not set |

| Key | Default | Meaning |
|---|---|---|
| `package` | the group and name of the package | The Kotlin package of the object |
| `object` | `BuildConfig` | Its name |
| `fields` | none | Constants of your own |
| `expose` | none | Variables from `[env]`, `.env` files and the shell |
| `build-time` | `false` | Also write `BUILD_TIME` |

`BUILD_TIME` is the value of `SOURCE_DATE_EPOCH` when it is set, else the current time. The current time changes on every build, so the file changes and the program is compiled again every time. Leave `build-time` off unless you need it, or set `SOURCE_DATE_EPOCH` (to the commit's time, for example) for builds that should be reproducible.

[`[env] expose`](../environment/#compile-variables-into-the-code) writes a `BuildConfig` of its own. With a `build-config` step for the same target, that list goes into the step's object instead, so there is one object, not two.

### Protocol Buffers and gRPC

`builtin = "protobuf"` downloads `protoc` from Maven Central for your platform (pinned in `kargo.lock`) and runs it on the `.proto` files:

```toml title="kargo.toml"
[dependencies]
"com.google.protobuf:protobuf-kotlin" = "4.31.1"

[[generate]]
name = "protos"
builtin = "protobuf"
```

The inputs are `proto/**/*.proto`, imports are searched in `proto/`, and the step generates the Java classes and the Kotlin DSL (`--java_out` and `--kotlin_out`) for the JVM and Android targets. The libraries the generated code uses are dependencies like any other: add `protobuf-java` or `protobuf-kotlin` at the version of `protoc`. With a message `Person` in `option java_package = "demo.proto"`:

```kotlin title="src/Main.kt"
import demo.proto.person

fun main() {
    val ada = person { name = "Ada"; id = 7 }
    println(ada)
}
```

For gRPC, set `grpc = true`. Kargo also downloads `protoc-gen-grpc-java` and, with the Kotlin DSL on, `protoc-gen-grpc-kotlin`, and generates the service stubs. Add the runtime libraries yourself:

```toml title="kargo.toml"
[dependencies]
"com.google.protobuf:protobuf-kotlin" = "4.31.1"
"io.grpc:grpc-protobuf" = "1.73.0"
"io.grpc:grpc-stub" = "1.73.0"
"io.grpc:grpc-kotlin-stub" = "1.4.3"
"org.jetbrains.kotlinx:kotlinx-coroutines-core" = "1.10.2"

[[generate]]
name = "protos"
builtin = "protobuf"
grpc = true
```

| Key | Default | Meaning |
|---|---|---|
| `version` | `4.31.1` | The `protoc` version |
| `proto-path` | `["proto"]` | Directories searched for imports (`-I`) |
| `java`, `kotlin` | `true` | Generate Java classes and the Kotlin DSL. The DSL wraps the Java classes, so it needs `java` |
| `grpc` | `false` | Also generate gRPC stubs |
| `grpc-java-version` | `1.73.0` | The version of `protoc-gen-grpc-java` |
| `grpc-kotlin-version` | `1.4.3` | The version of `protoc-gen-grpc-kotlin` |
| `args` | none | More arguments for `protoc` |
| `inputs` | `["proto/**/*.proto"]` | The `.proto` files to compile |

For a layout or a plugin the built-in does not cover, write the step yourself with `run` and a `protoc` you installed.

When you delete a `.proto` file or a message, the step runs again and the Java and Kotlin classes it produced are gone from the output, and from the compiled classes with them.

## Editors

`kargo metadata` lists every step (`generators`) and, for each target, the directories it compiles (`generated-roots`: the `roots` of each step, or its output directory). They are also in `source-roots`, `test-roots` and `resource-roots`, so the Kotlin server and the IDE plugins index the generated code like the rest. The directories exist after a build or a `kargo generate`; `kargo metadata` itself never runs a step. See [Editor integration](../editors/).

## What this does not do

- Steps read files; they do not look inside your Kotlin sources. Code that comes from annotations in your own code is the job of annotation processors and [KSP](../frameworks/).
- A step has no access to the compiler's classpath. It runs before anything is compiled.
- There are no built-ins yet for SQLDelight or Compose resources. [Compose resources](../compose/) already get their `Res` class generated, without a step.
