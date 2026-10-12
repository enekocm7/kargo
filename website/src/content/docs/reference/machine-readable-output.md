---
title: Machine-readable output
description: JSON output for compiler diagnostics, kargo info and kargo tree, and JUnit XML test reports, for editors, scripts and CI.
---

Kargo's normal output is for people. Editors, scripts and CI systems can ask for stable, machine-readable formats instead:

| You want | Use |
|---|---|
| Compiler errors and warnings with file, line and column | `kargo build --message-format json` (also `check`, `test`, `run`) |
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

`kargo build` and `kargo check` end with a `build-finished` message whose `success` says whether the build succeeded. The exit code is the same as without the flag: `101` when the build fails.

`--message-format` takes `human` (the default) or `json`. With `kargo test` and `kargo run` only the compiler output becomes JSON: the tests' and the program's own output still goes to stdout as text after it. Compiler output that isn't a diagnostic, such as Kotlin/Native's linker messages, goes to stderr. Android builds run through Gradle, so their diagnostics are not converted.

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
