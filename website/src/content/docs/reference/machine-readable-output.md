---
title: Machine-readable output
description: JSON output for compiler diagnostics, test discovery and test results, kargo metadata, kargo info, kargo tree, kargo why and kargo android devices, and JUnit XML test reports, for editors, scripts and CI.
---

Kargo's normal output is for people. Editors, scripts and CI systems can ask for stable, machine-readable formats instead:

| You want | Use |
|---|---|
| Compiler errors and warnings with file, line and column | `kargo build --message-format json` (also `check`, `test`, `run`) |
| What to do about a failed command (`help`, `suggestions`) | `--message-format json`, see [Errors](#errors) |
| The project model for an editor: roots, classpaths, compiler arguments | `kargo metadata` |
| The tests of a project, with their source locations | `kargo test --list --format json` |
| Each test's result as it runs, for a test explorer | `kargo test --message-format json` |
| Project metadata | `kargo info --format json` |
| The resolved dependency graph | `kargo tree --format json` |
| Why a module and its version are on the classpath | `kargo why <module> --format json` |
| Per-test results for CI | `kargo test --report junit` |
| The devices, emulators and virtual devices `kargo run -t android` can use | `kargo android devices --format json` |

JSON goes to stdout. Kargo's status lines (`Compiling`, `Finished`, ...) stay on stderr, so you can pipe stdout straight into a JSON parser.

## Compiler diagnostics

```console
$ kargo build --message-format json 2>/dev/null
{"column":16,"file":"/home/me/app/src/Main.kt","line":2,"message":"initializer type mismatch: expected 'Int', actual 'String'.","reason":"compiler-message","rendered":"src/Main.kt:2:16: error: initializer type mismatch: expected 'Int', actual 'String'.\n    val x: Int = \"nope\"\n               ^","severity":"error","target":"jvm"}
{"reason":"build-finished","success":false}
```

Each line is one JSON object; its `reason` says what it is.

A `compiler-message` is one diagnostic from the Kotlin compiler (JVM, Wasm, JS or Native) or `javac`:

| Field | Meaning |
|---|---|
| `severity` | `error`, `warning`, `info`, `exception` or `note` (javac's notes) |
| `message` | The message, without location or severity |
| `file` | Absolute path of the source file, or `null` for messages without a location |
| `line` | 1-based line, or `null` |
| `column` | 1-based column, or `null` (javac reports none) |
| `rendered` | The diagnostic as the compiler printed it, with the source line and a caret |
| `target` | The target being compiled: `jvm`, `wasm`, `js` or `native`. A source in `src/` is compiled for every target, so the same error can come once per target |

With [`warnings-as-errors`](../manifest/#kotlin), the compiler's `warnings found and -Werror specified` summary is the last `compiler-message`, after the diagnostics it counts. Kargo's own [warnings](../warnings/), such as a target without sources, are plain text on stderr, not JSON messages.

`kargo build` and `kargo check` end with a `build-finished` message whose `success` says whether the build succeeded, and, when it did not and Kargo has advice for the failure, the optional `help` and `suggestions` of [errors](#errors). The exit code is the same as without the flag: `101` when the build fails.

## Errors

A failure that is not a compiler diagnostic, such as a typo in `kargo.toml`, is one more message with `--message-format json`:

```console
$ kargo build --message-format json 2>/dev/null
{"help":"key `jvm-targt` in `[target.jvm]` is not known; did you mean `jvm-target`?","message":"failed to parse manifest at /home/me/app/kargo.toml: TOML parse error at line 6, column 1 ...","reason":"error","suggestions":["jvm-target"]}
```

| Field | Meaning |
|---|---|
| `message` | The error and its causes, joined with `: ` |
| `help` | What to do next: the `help:` lines of the human output, one per line. Absent when there is no advice |
| `suggestions` | The names that would fix the error, best first (the closest valid key, command, package, coordinate or version). Absent when there are none |

When the build itself fails, `build-finished` (below) has the `help` and `suggestions` fields instead, and no `error` message comes before it. Both fields are optional: read them when present.

`--message-format` takes `human` (the default) or `json`. With `kargo run` only the compiler output becomes JSON: the program's own output still goes to stdout as text after it. `kargo test` also reports each test as JSON ([Test results](#test-results)) and moves its human-readable report to stderr. Compiler output that isn't a diagnostic, such as Kotlin/Native's linker messages, goes to stderr. Android builds run through Gradle, so their diagnostics are not converted.

## Test discovery

`kargo test --list --format json` compiles the tests and prints one `test-discovered` message per test, without running any:

```console
$ kargo test --list --format json 2>/dev/null
{"display-name":"adds()","file":"/home/me/app/tests/MathTest.kt","id":"[engine:junit-jupiter]/[class:com.example.app.MathTest]/[method:adds()]","line":7,"name":"adds","package":"app","parent-id":"[engine:junit-jupiter]/[class:com.example.app.MathTest]","reason":"test-discovered","suite":"com.example.app.MathTest","target":"jvm"}
```

| Field | Meaning |
|---|---|
| `package` | The package the test belongs to |
| `target` | `jvm` or `native` |
| `id` | Identifies the test within its target. On the JVM it is the JUnit Platform unique id; on Native it is `suite.name` |
| `parent-id` | The id of the class (or other container) the test belongs to, or `null` (Native) |
| `suite` | The test class, such as `com.example.app.MathTest` (nested classes use `$`) |
| `name` | The test function, such as `adds` |
| `display-name` | The name the test framework shows, such as `adds()` |
| `file` | Absolute path of the source file that declares the test, or `null` if Kargo can't tell |
| `line` | 1-based line of the test function (or of its class when the function isn't found), or `null` |

A parameterized test or test factory is listed once; its invocations only exist when it runs, and their `parent-id` is its `id`. Kargo finds `file` and `line` by looking for the class and function in the test sources, so tests generated at run time (Kotest specs, dynamic tests) point at their class at best.

Wasm, JS and Android tests are not listed yet: `kargo test --list` notes that and prints nothing for them. A filter (`kargo test --list MathTest`) lists only the tests it would run.

## Test results

With `--message-format json`, `kargo test` reports each test as JSON while it runs, on the same stdout as the compiler messages:

```console
$ kargo test --message-format json 2>/dev/null
{"display-name":"adds()","id":"[engine:junit-jupiter]/[class:com.example.app.MathTest]/[method:adds()]","name":"adds","package":"app","parent-id":"[engine:junit-jupiter]/[class:com.example.app.MathTest]","reason":"test-started","suite":"com.example.app.MathTest","target":"jvm"}
{"display-name":"adds()","duration-ms":12,"id":"[engine:junit-jupiter]/[class:com.example.app.MathTest]/[method:adds()]","name":"adds","outcome":"passed","package":"app","parent-id":"[engine:junit-jupiter]/[class:com.example.app.MathTest]","reason":"test-finished","suite":"com.example.app.MathTest","target":"jvm"}
{"failed":0,"ignored":0,"package":"app","passed":1,"reason":"test-summary","success":true,"target":"jvm"}
```

`test-started` and `test-finished` carry the `package`, `target`, `id`, `parent-id`, `suite`, `name` and `display-name` of [test discovery](#test-discovery), so the ids match those of `--list` (an invocation of a parameterized test has its own id, with the parameterized test as its `parent-id`). `test-finished` adds:

| Field | Meaning |
|---|---|
| `outcome` | `passed`, `failed` or `ignored` (skipped, disabled or a failed assumption) |
| `duration-ms` | How long the test took, in milliseconds |
| `message` | Why it failed (the assertion message), or why it was skipped; absent when it passed |
| `details` | The stack trace of a failure |
| `stdout`, `stderr` | What the test printed, when it printed something (`stderr` on the JVM only) |

An ignored test gets a `test-finished` message without a `test-started` one. On the JVM, a class whose setup failed (such as a failing `@BeforeAll`) is reported as a failed `test-finished` whose `suite` is the class and whose `name` is its display name.

Each target ends with a `test-summary` message: the `passed`, `failed` and `ignored` counts and `success`. The exit code is the same as without the flag.

With `--message-format json`, stdout holds only JSON messages: the human-readable report (`test MathTest.adds ... ok`, JUnit's tree, the result table) moves to stderr. Native, Wasm and JS results come from the test runner's TeamCity messages as each test ends; Android results come from Gradle's JUnit XML once all tests have run, so there are no `test-started` messages.

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
  "default-members": ["app"],
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
          "test-roots": ["/home/me/app/tests", "/home/me/app/tests-jvm", "/home/me/app/test-fixtures", "/home/me/app/test-fixtures-jvm", "/home/me/app/benches", "/home/me/app/benches-jvm"],
          "test-fixture-roots": ["/home/me/app/test-fixtures", "/home/me/app/test-fixtures-jvm"],
          "bench-roots": ["/home/me/app/benches", "/home/me/app/benches-jvm"],
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
                  "path": "/home/me/.kargo/cache/org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21.jar",
                  "sources": "/home/me/.kargo/cache/org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21-sources.jar"
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
| `default-members` | The members commands work on at the workspace root: [`default-members`](../manifest/#workspace), else every member |
| `lockfile`, `target-dir` | The project's `kargo.lock` and build directory |
| `profile` | `dev`, `release` with `--release`, or the name given to `--profile`: the profile whose output directories and compiler arguments are shown |
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
| `path-dependencies` | Each path dependency: the manifest key (`name`), the `package` it points to, `scope`, `dev`, the `target` it is limited to (or `null`), and whether it is `exported` and brings its `test-fixtures`. Together they are the module graph |
| `bins` | The [`[[bin]]`](../manifest/#bin) tables, with `path` made absolute |
| `examples` | The `examples/<name>.kt` programs, with their `name` and `path` |
| `tasks` | Each [task](../../guides/tasks-and-plugins/): `name`, `description`, the commands it runs on this platform (`run`), its Kotlin `script` (an absolute path, else `null`) and `depends-on` |
| `hook-plugins` | Each hook plugin: `name`, its `command` (`null` for a script), its `script` (an absolute path, else `null`), `args` on this platform and `hooks` |
| `generators` | Each [`[[generate]]` step](../../guides/code-generation/): `name`, `kind`, `targets`, `inputs`, `after`, how it works (`run` with its commands, `tool` with the coordinates, or `builtin` with `build-config` or `protobuf`) its `output-dir`, `target/generated/<profile>/<name>`, and the `roots` in it that hold the sources or resources |
| `targets` | One entry per enabled target |

### Targets

| Field | Targets | Meaning |
|---|---|---|
| `kind` | all | `jvm`, `wasm`, `js`, `native` or `android` |
| `source-roots`, `test-roots` | all | `src/` and `src-<target>/`, then the directories of the target's `source-sets`, then the output of `kind = "sources"` steps; `tests/`, `tests-<target>/`, the test fixtures `test-fixtures/` and `test-fixtures-<target>/` and the benchmarks `benches/` and `benches-<target>/`, then the `tests-<set>/` and `test-fixtures-<set>/` of its `source-sets`, then the output of `kind = "test-sources"` steps, whether they exist or not |
| `source-sets` | all | The [source sets of several targets](../../guides/multiplatform/#source-sets-for-several-targets) (`[source-sets.<name>]`) this target compiles, widest first: `name`, `targets` (every target the set lists, enabled or not), `platforms` (the Konan platforms of a set of only some Native platforms, else empty), `refines` (the sets it refines directly; the target refines the narrowest ones) and the `source-root`, `test-root` and `test-fixture-root` directories, which are in the root lists too. Empty without `[source-sets]` |
| `generated-roots` | all | The output directories of the `[[generate]]` steps the target compiles, each with its `name`, `kind` (`sources`, `test-sources` or `resources`), `dir` and whether the files are `common` to every target. They are in `source-roots`, `test-roots` and `resource-roots` too, so an editor that reads those lists sees generated code. They exist after a build or `kargo generate`; `kargo metadata` runs no step |
| `test-fixture-roots` | all | `test-fixtures/` and `test-fixtures-<target>/` ([test fixtures](../../guides/workspaces/#test-fixtures)), which `test-roots` includes too |
| `bench-roots` | all | `benches/` and `benches-<target>/` ([benchmarks](../../guides/benchmarking/)), which `test-roots` includes too, so editors compile them like tests |
| `output-dir` | all | `target/<profile>/<target>` (with the member's name in a workspace) |
| `android` | Android | The Android module: its `kind` (`application` or `library`), `namespace` and `r-class` (`<namespace>.R`), the Android `manifest`, `res-roots` and `asset-roots`, the `library-dependencies` (the Android libraries among its path dependencies, which are Gradle subprojects of its build), the Gradle `build-dir`, the default `variant` (`debug` with the dev profile, after the first flavor of each dimension), and where AGP writes generated code for that variant once Gradle built it: `generated-source-roots` (the `BuildConfig` directory, empty without `build-config` fields) and the `r-class-jar`. `variant`, `generated-source-roots` and `r-class-jar` are `null` (or empty) for a profile Android does not build |
| `compiler-args` | not Android | The Kotlin compiler arguments every compilation of the target gets: `[kotlin]` and profile settings plus the frameworks' flags |
| `compiler-plugins` | not Android | Each compiler plugin (the frameworks' and [`[plugins]`](../manifest/#pluginsname)): `module`, `version`, `id`, its `-P` `options` and the downloaded `jar` |
| `resource-roots`, `test-resource-roots`, `classes-dir`, `test-classes-dir` | JVM | Resource directories (`resources/`, then the output of `kind = "resources"` steps) and where classes are compiled to |
| `compose-resources` | JVM, Wasm, Native | With `[frameworks] compose` and a `composeResources/` directory: the `package` of the generated `Res` class, the `source-root` that holds `Res.kt` and the `resource-root` put on the classpath. `kargo metadata` writes them, so editors resolve `Res` before a build. `null` otherwise |
| `previews` | JVM | The `@Preview` composables `kargo preview` renders, each with its qualified `name`, `function`, JVM `class`, `path` and 1-based `line`. Empty without `[frameworks] compose` |
| `jvm-target`, `main-class`, `jvm-args`, `javac-args` | JVM | The bytecode version the build uses, the main class (`[target.jvm] main-class`, or the single `main` in the sources, else `null`), and the extra JVM and javac options |
| `environment`, `wasm-kind`, `module-name` | Wasm | `[target.wasm]` settings |
| `environment`, `js-kind`, `module-kind`, `module-name` | JS | `[target.js]` settings |
| `npm` | Wasm, JS | The npm packages the program needs, from the klibs of its dependencies and `[target.wasm.npm-dependencies]`: each with its `name`, `version` and `directory`, where `kargo build` installs it (`target/<profile>/node_modules/<name>`) whether or not it is installed yet. `declared` is the version `kargo.toml` gives the package (`null` when only libraries ask for it) and `requested-by` lists the libraries whose klibs ask for it (`module`, `version`, and the `spec` they want), so you can tell a package your manifest overrides from one a library brought. `test` is `true` for a package only the tests need (`dev-npm-dependencies` or a test library's): `kargo test` installs those into `target/<profile>/wasm/test/node_modules`, and only when they differ from the program's packages. The JS target lists its packages the same way, from `[target.js.npm-dependencies]`, installed in `target/<profile>/js/node_modules` (`js/test/node_modules` for the tests'). The packages of a path dependency are missing, since their klibs aren't built to be read. `null` with `--no-deps` |
| `npm-declarations` | Wasm, JS | Where the build writes the [Kotlin declarations](../../guides/wasm/#kotlin-declarations-for-npm-packages) of `[target.wasm.npm-dependencies]` (`target/<profile>/generated/npm`) or `[target.js.npm-dependencies]` (`generated/npm-js`), whether or not they exist yet; editors load the Kotlin files under it as a library of that target. `null` without npm dependencies (and on JS before Kotlin 2.2.20), also with `--no-deps` |
| `js-sources` | Wasm, JS | The [`js-sources`](../../guides/wasm/#your-own-javascript-and-typescript) directories of the target that exist (`js/` by default) |
| `js-declarations` | Wasm, JS | Where the build and `kargo lsp` write the Kotlin declarations of the TypeScript modules in `js-sources` (`target/<profile>/generated/js-sources`, `generated/js-sources-js` for JS); editors load them like `npm-declarations`. `null` without `js-sources` directories |
| `native-kind`, `platforms`, `entry-point` | Native | `[target.native]` settings. `host` in `platforms` becomes the host's platform, and the entry point is detected as the build does |
| `dependencies` | all | One entry per platform (`jvm`, `wasm-js`, `js`, `native-linux_x64`, ...). `null` with `--no-deps`, and for Android, whose dependencies Gradle resolves |

Each platform in `dependencies` has three lists:

| List | JVM | Wasm, JS, Native |
|---|---|---|
| `compile` | Compiles the main sources: `compile` and `provided` dependencies | Every library the main sources compile and link against |
| `runtime` | Runs the program: `compile` and `runtime` dependencies | The same as `compile` |
| `test` | Compiles and runs the tests: all of the above plus `[dev-dependencies]` | `compile` plus `[dev-dependencies]` |

The entries are in classpath order. A path dependency is `{ "package": "<name>", "scope": "..." }`; use that package's roots. A Maven artifact has its `module`, the resolved `version`, the downloaded file's `path`, its `sources` jar if that is in Kargo's cache, else `null`, and, for an artifact without a sources jar, its `javadoc` jar if that is in the cache, else `null`. [`kargo fetch --sources`](../commands/#kargo-fetch) downloads the sources jars, and the Javadoc jars of libraries without one.

## `kargo info --format json`

```console
$ kargo info --format json
{
  "bins": [
    { "entry-point": null, "main": null, "name": "migrate", "path": "src/tools/Migrate.kt" }
  ],
  "cache": { "path": "/home/me/.kargo/cache", "bytes": 356793344 },
  "daemons": 0,
  "dependencies": [
    {
      "module": "org.jetbrains.kotlinx:kotlinx-datetime",
      "name": "org.jetbrains.kotlinx:kotlinx-datetime",
      "scope": "compile",
      "version": "0.7.1"
    }
  ],
  "dev-dependencies": [],
  "environment": { "JAVA_HOME": "/usr/lib/jvm/jdk-21", "KARGO_HOME": "/home/me/.kargo" },
  "examples": ["greet"],
  "framework-versions": [{ "managed": [], "name": "serialization", "version": "1.11.0" }],
  "frameworks": ["serialization"],
  "home": { "path": "/home/me/.kargo", "bytes": 1288490188 },
  "jdk": "system",
  "jdk-toolchain": "JDK 21 at /usr/lib/jvm/jdk-21 (JAVA_HOME)",
  "kargo": { "version": "0.1.4", "commit": "abc1234", "built": "2026-10-31", "target": "x86_64-unknown-linux-gnu" },
  "kotlin": "2.4.21",
  "manifest": "/home/me/app/kargo.toml",
  "package": {
    "description": null,
    "group": "com.example",
    "name": "app",
    "version": "0.1.0"
  },
  "repositories": [
    { "name": "maven-central", "url": "https://repo.maven.apache.org/maven2", "effective-url": "https://repo.maven.apache.org/maven2" }
  ],
  "root": "/home/me/app",
  "target-dir": "/home/me/app/target",
  "targets": ["jvm"],
  "toolchains": "JDK jdk-21.0.5+11, Gradle 9.6.0",
  "version": 1
}
```

`version` is the format version: fields are added freely, and removing or redefining one makes it 2. `kargo` is the build of the binary (`version`, `commit`, `built`, and the `target` triple), `jdk-toolchain` is the JDK the project builds with and where it came from, `toolchains` is what is installed under the Kargo home, and `home` and `cache` are their paths and sizes in bytes (`bytes` is `null` when the directory does not exist). `environment` holds the [environment variables Kargo reads](../environment/) that are set, with the home directory shortened to `~` and credentials removed; the variables that hold credentials (`NPM_TOKEN`, `KARGO_SIGNING_KEY`, `GITHUB_TOKEN`, repository passwords) are never included.

At the root of a [workspace](../../guides/workspaces/), it describes the workspace instead: `workspace` (its root), `members` (each with its `name`, `version`, `root` and the `dependencies` it builds on through path dependencies), `default-members`, `lockfile` and `target-dir`, plus `version`, `kargo`, `environment`, `home`, `cache` and `daemons`.

`framework-versions` has an entry for each enabled framework, in the order of `frameworks`: its `name`, the `version` it uses, and the `managed` modules of its BOM that `kargo.lock` resolved for the JVM runtime, each with its `name`, `module` and `version`. `managed` is empty for a framework without a BOM, and before the project has been built or fetched. For Spring Boot these are Spring Framework, Spring Security and Jackson.

`dependencies` and `dev-dependencies` are what `kargo.toml` declares: `name` is the key in `kargo.toml`, `version` is `null` when a BOM manages it. Target-specific dependencies are not listed; `kargo tree --format json` shows everything that was resolved.

`bins` are the [`[[bin]]`](../manifest/#bin) tables as written (unset keys are `null`), and `examples` the names of the `examples/<name>.kt` programs: what `kargo run --bin` and `kargo run --example` accept.

## `kargo classpath --format json`

```json
{
  "package": "my-app",
  "target": "jvm",
  "platform": "jvm",
  "scope": "compile",
  "classpath": [
    "/home/me/.kargo/cache/org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21.jar"
  ]
}
```

| Field | Meaning |
|---|---|
| `package` | The package's name |
| `target` | `jvm`, `wasm`, `js` or `native` |
| `platform` | `jvm`, `wasm-<environment>` (`wasm-js`, `wasm-wasi`), `js` or `native-<platform>`, the platform whose libraries are listed |
| `scope` | `compile`, `runtime` or `test` |
| `classpath` | The jars or klibs, in the order the compiler gets them: path dependencies, then Maven artifacts |

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
          "files": ["/home/me/.kargo/cache/org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21.jar"],
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

`--format json` can't be combined with `--depth`, `--duplicates` or `--invert`; `kargo why --format json --all-paths` gives the paths to one module.

## `kargo why --format json`

```console
$ kargo why g:util --format json
{
  "configuration": "runtime",
  "module": "g:util",
  "package": { "name": "app", "version": "0.1.0" },
  "results": [
    {
      "target": "jvm",
      "module": "g:util",
      "version": "2.0",
      "reason": { "kind": "highest" },
      "requests": [
        { "version": "1.0", "selected": false, "from": { "kind": "dependency", "module": "g:left", "version": "1.0" } },
        { "version": "2.0", "selected": true, "from": { "kind": "dependency", "module": "g:right", "version": "1.0" } }
      ],
      "paths": [["g:left:1.0", "g:util:2.0"]],
      "truncated": false
    }
  ]
}
```

`results` has one entry per resolution that has the module: `jvm`, `wasm-js`, `js`, `native-<platform>`. Several packages print an array with one document per package.

| Field | Meaning |
|---|---|
| `version` | The version on the classpath |
| `reason.kind` | `highest`, `forced`, `range` (with the `ranges`), `pinned`, or `locked` when `kargo.lock` holds a version that resolving again would not pick; then `fresh-version` and `fresh-reason` say which one and why |
| `requests` | Every version asked for: `version` as written (a range stays a range), whether it is the `selected` one, and `from` |
| `from.kind` | `project` (with `force`), `dependency`, `relocation`, `variant`, `constraint` (each with the `module` and `version` that asked), `platform` (also with the `platform` and `platform-version`) or `bom` (the BOM's `module` and `version`) |
| `paths` | Each path from a dependency in `kargo.toml` to the module, as `group:artifact:version`: the shortest one, or all of them (at most 100) with `--all-paths` |
| `truncated` | Whether there were more paths than were listed |

## `kargo android devices --format json`

The devices `kargo run -t android` can target. Nothing is started or installed to list them.

```json
{
  "devices": [
    { "id": "emulator-5554", "name": "Pixel_8_API_35", "kind": "emulator", "state": "device", "api": 35, "avd": "Pixel_8_API_35" },
    { "id": "R58M123ABC", "name": "samsung SM-S918B", "kind": "physical", "state": "unauthorized", "api": null, "avd": null },
    { "id": "Tablet_API_34", "name": "Tablet_API_34", "kind": "avd", "state": "stopped", "api": 34, "avd": null }
  ]
}
```

| Field | Meaning |
|---|---|
| `id` | The serial of a connected device (`emulator-5554`, or `host:port` over Wi-Fi), or the name of a virtual device that is not running. This is what `--device` takes |
| `name` | The maker and model of a device, or the name of the virtual device |
| `kind` | `physical`, `emulator` (running) or `avd` (installed, not running) |
| `state` | `adb`'s: `device` (ready), `unauthorized`, `offline`, `no permissions`, ...; `stopped` for a virtual device that is not running |
| `api` | The Android API level, when known (`null` for a device that is not ready) |
| `avd` | The virtual device a running emulator runs, else `null` |

`kargo android avd list --format json` prints `{"avds": [...]}` with `name`, `api`, `image` (the system image package), `hardware` and `path`.

## JUnit XML test reports

```sh
kargo test --report junit
```

writes JUnit XML reports to `target/<profile>/<target>/test-reports/` for every target, alongside the normal console output. See [Test reports for CI](../../guides/testing/#test-reports-for-ci) for the files each target writes and [CI](../../guides/ci/#test-results) for publishing them.
