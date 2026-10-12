---
title: Machine-readable output
description: JSON output for compiler diagnostics, kargo metadata, kargo info and kargo tree, and JUnit XML test reports, for editors, scripts and CI.
---

Kargo's normal output is for people. Editors, scripts and CI systems can ask for stable, machine-readable formats instead:

| You want | Use |
|---|---|
| Compiler errors and warnings with file, line and column | `kargo build --message-format json` (also `check`, `test`, `run`) |
| The project model for an editor: roots, classpaths, compiler arguments | `kargo metadata` |
| Project metadata | `kargo info --format json` |
| The resolved dependency graph | `kargo tree --format json` |
| Per-test results for CI | `kargo test --report junit` |

JSON goes to stdout. Kargo's status lines (`Compiling`, `Finished`, ...) stay on stderr, so you can pipe stdout straight into a JSON parser.

## Compiler diagnostics

```console
$ kargo build --message-format json 2>/dev/null
{"column":16,"file":"/home/me/app/src/Main.kt","line":2,"message":"initializer type mismatch: expected 'Int', actual 'String'.","reason":"compiler-message","rendered":"src/Main.kt:2:16: error: initializer type mismatch: expected 'Int', actual 'String'.\n    val x: Int = \"nope\"\n               ^","severity":"error"}
{"reason":"build-finished","success":false}
```

Each line is one JSON object; its `reason` says what it is.

A `compiler-message` is one diagnostic from the Kotlin compiler (JVM, Wasm or Native) or `javac`:

| Field | Meaning |
|---|---|
| `severity` | `error`, `warning`, `info`, `exception` or `note` (javac's notes) |
| `message` | The message, without location or severity |
| `file` | Absolute path of the source file, or `null` for messages without a location |
| `line` | 1-based line, or `null` |
| `column` | 1-based column, or `null` (javac reports none) |
| `rendered` | The diagnostic as the compiler printed it, with the source line and a caret |

With [`warnings-as-errors`](../manifest/#kotlin), the compiler's `warnings found and -Werror specified` summary is the last `compiler-message`, after the diagnostics it counts. Kargo's own [warnings](../warnings/), such as a target without sources, are plain text on stderr, not JSON messages.

`kargo build` and `kargo check` end with a `build-finished` message whose `success` says whether the build succeeded. The exit code is the same as without the flag: `101` when the build fails.

`--message-format` takes `human` (the default) or `json`. With `kargo test` and `kargo run` only the compiler output becomes JSON: the tests' and the program's own output still goes to stdout as text after it. Compiler output that isn't a diagnostic, such as Kotlin/Native's linker messages, goes to stderr. Android builds run through Gradle, so their diagnostics are not converted.

## `kargo metadata`

`kargo metadata` describes the whole project so that editors and language servers can import it without reading `kargo.toml` themselves. See [Editor integration](../../guides/editors/) for how to use it.

```console
$ kargo metadata
{
  "version": 1,
  "kargo-version": "0.1.0",
  "fingerprint": "0fc1ba90f93965e4afe82ae2486d46036a51dd0a0c9fc25a1aa75d377820a20d",
  "watch": ["/home/me/app/kargo.toml", "/home/me/app/kargo.lock"],
  "root": "/home/me/app",
  "workspace": false,
  "members": ["app"],
  "lockfile": "/home/me/app/kargo.lock",
  "target-dir": "/home/me/app/target",
  "profile": "dev",
  "resolved": true,
  "toolchain": {
    "kotlin": "2.4.21",
    "jdk": { "request": "system", "home": "/usr/lib/jvm/temurin-25", "version": 25 },
    "kotlin-native": null
  },
  "packages": [
    {
      "name": "app",
      "version": "0.1.0",
      "group": "com.example",
      "description": null,
      "member": true,
      "root": "/home/me/app",
      "manifest": "/home/me/app/kargo.toml",
      "lockfile": "/home/me/app/kargo.lock",
      "kotlin": { "version": "2.4.21", "language-version": null, "api-version": null, "opt-in": [], "warnings-as-errors": false },
      "frameworks": [],
      "path-dependencies": [],
      "bins": [],
      "examples": [],
      "tasks": [],
      "targets": [
        {
          "kind": "jvm",
          "source-roots": ["/home/me/app/src", "/home/me/app/src-jvm"],
          "test-roots": ["/home/me/app/tests", "/home/me/app/tests-jvm"],
          "resource-roots": ["/home/me/app/resources"],
          "test-resource-roots": ["/home/me/app/test-resources"],
          "output-dir": "/home/me/app/target/debug/jvm",
          "classes-dir": "/home/me/app/target/debug/jvm/classes",
          "test-classes-dir": "/home/me/app/target/debug/jvm/test-classes",
          "jvm-target": "25",
          "main-class": "com.example.app.MainKt",
          "jvm-args": [],
          "javac-args": [],
          "compiler-args": [],
          "compiler-plugins": [],
          "dependencies": [
            {
              "platform": "jvm",
              "compile": [
                {
                  "module": "org.jetbrains.kotlin:kotlin-stdlib",
                  "version": "2.4.21",
                  "path": "/home/me/.kargo/cache/maven/org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21.jar",
                  "sources": "/home/me/.kargo/cache/maven/org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21-sources.jar"
                },
                ...
              ],
              "runtime": [...],
              "test": [...]
            }
          ]
        }
      ]
    }
  ]
}
```

Kargo prints the keys of every object in alphabetical order; the example above is reordered for reading. Paths are absolute.

### Versioning

`version` is the format version, `1`. It changes only when a field is removed or changes its meaning. New fields can appear in any release, so ignore the ones you don't know.

### Staying up to date

The metadata depends only on the files in `watch`: every `kargo.toml` of the project (the workspace's included) and `kargo.lock`. `fingerprint` is a hash of their contents. Watch those files, and when one changes, compare `kargo metadata --fingerprint` with the fingerprint you last imported. It resolves nothing and returns at once.

### The project

| Field | Meaning |
|---|---|
| `root` | The workspace root, or the package's directory |
| `workspace` | Whether the project is a [workspace](../../guides/workspaces/) |
| `members` | The names of the workspace members (the single package otherwise) |
| `lockfile`, `target-dir` | The project's `kargo.lock` and build directory |
| `profile` | `dev`, or `release` with `--release`: the profile whose output directories and compiler arguments are shown |
| `resolved` | `false` with `--no-deps`. Then every `dependencies` is `null`, `toolchain.jdk.home` is `null` and plugins have no `jar` |
| `toolchain.kotlin` | The Kotlin version |
| `toolchain.jdk` | The `[toolchain] jdk` request, and the `home` and major `version` of the JDK it resolves to (`null` when there is none) |
| `toolchain.kotlin-native` | The installed Kotlin/Native distribution (`version`, `home`), or `null` until it is installed |

### Packages

`packages` lists the members first, then the packages they reach through [path dependencies](../../guides/workspaces/), whose `member` is `false`.

| Field | Meaning |
|---|---|
| `name`, `version`, `group`, `description` | From `[package]` |
| `root`, `manifest` | The package's directory and `kargo.toml` |
| `kotlin` | `[kotlin]` settings: `version`, `language-version`, `api-version`, `opt-in`, `warnings-as-errors` |
| `frameworks` | The [built-in frameworks](../../guides/frameworks/) that are enabled |
| `path-dependencies` | Each path dependency: the manifest key (`name`), the `package` it points to, `scope`, `dev` and the `target` it is limited to (or `null`) |
| `bins` | The [`[[bin]]`](../manifest/#bin) tables, with `path` made absolute |
| `examples` | The `examples/<name>.kt` programs, with their `name` and `path` |
| `tasks` | Each [task](../../guides/tasks-and-plugins/): `name`, `description`, the commands it runs on this platform (`run`) and `depends-on` |
| `targets` | One entry per enabled target |

### Targets

| Field | Targets | Meaning |
|---|---|---|
| `kind` | all | `jvm`, `wasm`, `native` or `android` |
| `source-roots`, `test-roots` | all | `src/` and `src-<target>/`, `tests/` and `tests-<target>/`, whether they exist or not |
| `output-dir` | all | `target/<profile>/<target>` (with the member's name in a workspace) |
| `compiler-args` | not Android | The Kotlin compiler arguments every compilation of the target gets: `[kotlin]` and profile settings plus the frameworks' flags |
| `compiler-plugins` | not Android | Each compiler plugin (the frameworks' and [`[plugins]`](../manifest/#pluginsname)): `module`, `version`, `id`, its `-P` `options` and the downloaded `jar` |
| `resource-roots`, `test-resource-roots`, `classes-dir`, `test-classes-dir` | JVM | Resource directories and where classes are compiled to |
| `jvm-target`, `main-class`, `jvm-args`, `javac-args` | JVM | The bytecode version the build uses, the main class (`[target.jvm] main-class`, or the single `main` in the sources, else `null`), and the extra JVM and javac options |
| `environment`, `wasm-kind`, `module-name` | Wasm | `[target.wasm]` settings |
| `native-kind`, `platforms`, `entry-point` | Native | `[target.native]` settings. `host` in `platforms` becomes the host's platform, and the entry point is detected as the build does |
| `dependencies` | all | One entry per platform (`jvm`, `wasm-js`, `native-linux_x64`, ...). `null` with `--no-deps`, and for Android, whose dependencies Gradle resolves |

Each platform in `dependencies` has three lists:

| List | JVM | Wasm, Native |
|---|---|---|
| `compile` | Compiles the main sources: `compile` and `provided` dependencies | Every library the main sources compile and link against |
| `runtime` | Runs the program: `compile` and `runtime` dependencies | The same as `compile` |
| `test` | Compiles and runs the tests: all of the above plus `[dev-dependencies]` | `compile` plus `[dev-dependencies]` |

The entries are in classpath order. A path dependency is `{ "package": "<name>", "scope": "..." }`; use that package's roots. A Maven artifact has its `module`, the resolved `version`, the downloaded file's `path`, and its `sources` jar if that is in Kargo's cache, else `null`. [`kargo fetch --sources`](../commands/#kargo-fetch) downloads the sources jars.

## `kargo info --format json`

```console
$ kargo info --format json
{
  "bins": [
    { "entry-point": null, "main": null, "name": "migrate", "path": "src/tools/Migrate.kt" }
  ],
  "dependencies": [
    {
      "module": "org.jetbrains.kotlinx:kotlinx-datetime",
      "name": "org.jetbrains.kotlinx:kotlinx-datetime",
      "scope": "compile",
      "version": "0.7.1"
    }
  ],
  "dev-dependencies": [],
  "examples": ["greet"],
  "frameworks": ["serialization"],
  "jdk": "system",
  "kotlin": "2.4.21",
  "manifest": "/home/me/app/kargo.toml",
  "package": {
    "description": null,
    "group": "com.example",
    "name": "app",
    "version": "0.1.0"
  },
  "repositories": [
    { "name": "maven-central", "url": "https://repo.maven.apache.org/maven2" }
  ],
  "root": "/home/me/app",
  "target-dir": "/home/me/app/target",
  "targets": ["jvm"]
}
```

`dependencies` and `dev-dependencies` are what `kargo.toml` declares: `name` is the key in `kargo.toml`, `version` is `null` when a BOM manages it. Target-specific dependencies are not listed; `kargo tree --format json` shows everything that was resolved.

`bins` are the [`[[bin]]`](../manifest/#bin) tables as written (unset keys are `null`), and `examples` the names of the `examples/<name>.kt` programs: what `kargo run --bin` and `kargo run --example` accept.

## `kargo tree --format json`

```console
$ kargo tree --format json
{
  "configuration": "runtime",
  "package": { "name": "app", "version": "0.1.0" },
  "trees": [
    {
      "target": "jvm",
      "roots": ["org.jetbrains.kotlin:kotlin-stdlib"],
      "modules": [
        {
          "id": "org.jetbrains.kotlin:kotlin-stdlib",
          "group": "org.jetbrains.kotlin",
          "artifact": "kotlin-stdlib",
          "version": "2.4.21",
          "files": ["/home/me/.kargo/cache/maven/org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21.jar"],
          "dependencies": [
            { "id": "org.jetbrains:annotations", "requested": "13.0", "version": "13.0" }
          ]
        },
        ...
      ]
    }
  ]
}
```

There is one entry in `trees` per target (one per platform for Native), for the configuration chosen with `-c` (`runtime` by default). Instead of a nested tree, `modules` lists every resolved module once, in classpath order, with its direct dependencies. Each dependency gives the version that was `requested` and the `version` that was selected, so conflicts show as a difference between the two. `files` are the downloaded artifacts in Kargo's cache.

`--format json` can't be combined with `--depth` or `--duplicates`.

## JUnit XML test reports

```sh
kargo test --report junit
```

writes JUnit XML reports to `target/<profile>/<target>/test-reports/` for every target, alongside the normal console output. See [Test reports for CI](../../guides/testing/#test-reports-for-ci) for the files each target writes and [CI](../../guides/ci/#test-results) for publishing them.
