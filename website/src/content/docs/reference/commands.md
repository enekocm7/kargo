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
| `--system-gradle` | Run Android builds with the `gradle` on `PATH` instead of a Kargo-managed Gradle (also `KARGO_SYSTEM_GRADLE=1`). A project `gradlew` still wins |
| `--manifest-path <PATH>` | Use this `kargo.toml` instead of searching from the current directory |
| `-V`, `--version` | Print the version |
| `-h`, `--help` | Print help |

**Exit codes:** `0` on success, `101` when a build or test fails or Kargo reports an error (as in Cargo). `kargo run` exits with your program's exit code.

Targets are `jvm`, `wasm`, `native` and `android`.

## Package selection

In a [workspace](../../guides/workspaces/), commands apply to every member at the workspace root and to the current member inside a member's directory. These options pick packages explicitly:

| Option | Commands | Meaning |
|---|---|---|
| `-p, --package <name>` | `build`, `check`, `test`, `doc`, `fmt`, `lint`, `tree`, `fetch`, `publish` | Only this package (repeatable) |
| `--workspace` | the same | Every member of the workspace |
| `-p, --package <name>` | `run`, `package`, `add`, `remove`, `outdated`, `upgrade` | The one package to work on; required at the root of a workspace with several members |

`-p` takes any package of the project, including a path dependency that is not a member. Outside a workspace there is one package, so you never need these options.

### Concurrent commands

Commands that build or resolve a project (`build`, `check`, `run`, `test`, `doc`, `fmt`, `lint`, `tree`, `classpath`, `fetch`, `metadata`, `ide`, `package`, `publish`, `update`, `clean`, `android`, `toolchain which`, and `lsp` while it imports the project) take an exclusive lock on `target/.kargo-lock` first. That lock guards both `target/` and `kargo.lock`. If another Kargo process (a second terminal, an IDE or a file watcher) holds the lock, the command waits for it to finish:

```console
$ kargo build
    Blocking waiting for file lock on build directory
   Compiling my-app v0.1.0 (jvm)
```

`kargo run` releases the lock once the build is done, so a long-running program doesn't block other commands. Downloads to the shared cache in `~/.kargo` don't need the lock: each file is written to a temporary name and renamed into place, so two projects can fetch the same artifact at the same time.

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

Same as `kargo new`, but in an existing directory (the current one by default). Existing files are not overwritten. A directory that already has a `kargo.toml` is refused; use [`kargo target add`](#kargo-target) to enable more targets in it.

```sh
kargo init [path] [options]
```

## `kargo build` (`b`)

Compile all enabled targets, or the ones you name. Targets build at the same time; if one fails, the command reports that failure and exits with code 101.

| Option | Meaning |
|---|---|
| `--keep-going` | Build every target even if one fails, then report each failure and which targets failed and succeeded |
| `--message-format <fmt>` | `human` (default) or `json`: print each compiler diagnostic as a JSON object on stdout. See [Machine-readable output](../machine-readable-output/#compiler-diagnostics) |
| `-r, --release` | Use the release profile |
| `-t, --target <target>` | Only this target (repeatable) |
| `-p, --package <name>`, `--workspace` | Which packages to build ([package selection](#package-selection)) |

[Path dependencies](../../guides/workspaces/) are built first, for the same targets.

Before compiling, `kargo build` (like `check`, `run`, `test` and `package`) [warns](../warnings/) about projects that are valid but probably not what you meant: a target with no sources, Kotlin scripts (`.kts`) that are never compiled, a [task](../../guides/tasks-and-plugins/) hidden by a built-in command, and an odd package version.

For Wasm, a release build removes dead code, runs `wasm-opt` when it is installed and prints the size of the module. A `kind = "library"` project also gets a `package.json` and TypeScript declarations. See [WebAssembly](../../guides/wasm/).

```sh
kargo build
kargo build --release -t jvm -t native
kargo build -p calc
kargo build --message-format json
```

With `--keep-going`, every failed target gets its own error and a summary follows:

```console
$ kargo build --keep-going
error: could not build the wasm target: ...
error: could not build 1 of 3 targets (wasm); jvm, native succeeded
```

## `kargo check` (`c`)

Type-check all enabled targets, or the ones you name, and report compiler errors and warnings without packaging or linking. It is the fastest feedback loop for editors and CI lint stages.

| Option | Meaning |
|---|---|
| `--tests` | Also check the test sources (`tests/`, `tests-<target>/`) |
| `--keep-going` | Check every target even if one fails, then report each failure (as for `kargo build`) |
| `--message-format <fmt>` | `human` (default) or `json`, as for `kargo build` |
| `-r, --release` | Use the release profile |
| `-t, --target <target>` | Only this target (repeatable) |
| `-p, --package <name>`, `--workspace` | Which packages to check |

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
| `-p, --package <name>` | The package to run; required at the root of a workspace with several members |
| `--main <name>` | Run another `main`: a JVM class such as `com.example.tools.MigrateKt`, or on Native an entry function such as `com.example.tools.main`. `--entry-point` is an alias |
| `--bin <name>` | Run the [`[[bin]]`](../manifest/#bin) with this name |
| `--example <name>` | Compile and run `examples/<name>.kt` against the project and its dev-dependencies |
| `--port <port>` | Wasm browser apps: the port to serve on (default `8080`; `0` picks a free one) |
| `--open` | Wasm browser apps: open the served page in your browser |
| `--debug[=<port>]` | JVM: start the program suspended until a debugger attaches over JDWP to `127.0.0.1:<port>` (default `5005`; `0` lets the JVM pick and print one). See [Debugging](../../guides/editors/#debugging) |
| `--message-format <fmt>` | `human` (default) or `json` for the compiler diagnostics, as for `kargo build`. The program's own output still goes to stdout |

`--main`, `--bin` and `--example` exclude each other and work for the JVM and Native targets. Without them, `kargo run` starts `main-class` / `entry-point` or the single `main` in the sources; with several, it lists them and any bins. See [Several programs in one project](../../guides/multiple-programs/).

`--port` and `--open` only apply to a Wasm browser app; `kargo run` rejects them for any other target. A Wasm project with `kind = "library"` has no program, so `kargo run` rejects it too.

How each target runs:

| Target | Runs |
|---|---|
| JVM | `java -cp ... <main-class>` with `jvm-args` |
| Wasm | `node target/<profile>/wasm/<name>.mjs`; the arguments reach `main(args)` (see [Program arguments](../../guides/wasm/#program-arguments)). With `environment = "browser"` it serves `target/<profile>/wasm/` on `127.0.0.1` until you press Ctrl+C (see [Browser apps](../../guides/wasm/#browser-apps)) |
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
| `--no-fail-fast` | Test every target even if one fails, then print a result per target. Overrides [`[test] fail-fast`](../manifest/#test) |
| `--report junit` | Also write JUnit XML reports to `target/<profile>/<target>/test-reports/` for CI systems. See [Test reports for CI](../../guides/testing/#test-reports-for-ci) |
| `--debug[=<port>]` | Start the JVM tests suspended until a debugger attaches over JDWP to `127.0.0.1:<port>` (default `5005`). Needs `-t jvm` in a project with several targets. See [Debugging tests](../../guides/testing/#debugging-tests) |
| `--list` | Compile the tests and print them (`<target>: <Class>.<name>`) instead of running them. JVM and Native only. See [Listing tests](../../guides/testing/#listing-tests) |
| `--format <fmt>` | With `--list`: `text` (default) or `json`, one [`test-discovered`](../machine-readable-output/#test-discovery) object per test |
| `--message-format <fmt>` | `human` (default) or `json`: compiler diagnostics and each test's start and result as [JSON messages](../machine-readable-output/#test-results) on stdout; the test report moves to stderr |
| `-r, --release` | Use the release profile |
| `-t, --target <target>` | Only this target (repeatable) |
| `-p, --package <name>`, `--workspace` | Which packages to test, one after another. The command fails if any test failed |

By default `kargo test` stops at the first target whose tests fail or don't compile, and notes which targets it skipped. See [Testing every target](../../guides/testing/#testing-every-target).

Wasm tests run on Node.js, or with `environment = "browser"` in a headless browser: Chrome, Edge, Chromium, Brave or Firefox, found on `PATH` or named by `KARGO_BROWSER`. See [Testing in a browser](../../guides/wasm/#testing-in-a-browser).

## `kargo doc` (`d`)

Generate HTML API documentation from KDoc comments with Dokka, into `target/debug/doc/`.

```sh
kargo doc [--open] [-t <target>]
```

| Option | Meaning |
|---|---|
| `--open` | Open the documentation in a browser when it is generated (the browser in `BROWSER`, else the system default) |
| `-t, --target <target>` | Only this target (repeatable); defaults to every enabled target |
| `-p, --package <name>`, `--workspace` | Which packages to document. In a workspace, each goes to `target/debug/<name>/doc/`, and `--open` opens the first |

Configure it with [`[doc]`](../manifest/#doc). See [API documentation](../../guides/documentation/).

## `kargo fmt`

Format the Kotlin files in `src/`, `src-<target>/`, `tests/` and `tests-<target>/` with ktlint.

```sh
kargo fmt [--check]
```

| Option | Meaning |
|---|---|
| `--check` | Report unformatted code without changing files; exit with code 101 if there is any |
| `-p, --package <name>`, `--workspace` | Which packages to format |

Without `--check`, files are rewritten in place, and the command exits with code 101 if ktlint found problems it can't fix. ktlint reads `.editorconfig`; configure the rest with [`[fmt]`](../manifest/#fmt). See [Formatting and linting](../../guides/formatting-and-linting/).

## `kargo lint`

Analyze the Kotlin files in `src/`, `src-<target>/`, `tests/` and `tests-<target>/` with detekt. Exits with code 101 if detekt reports issues.

```sh
kargo lint [--create-baseline]
```

| Option | Meaning |
|---|---|
| `--create-baseline` | Record the current issues in the baseline file (`detekt-baseline.xml` by default) instead of reporting them |
| `-p, --package <name>`, `--workspace` | Which packages to analyze |

detekt reads `detekt.yml`; configure the rest with [`[lint]`](../manifest/#lint). See [Formatting and linting](../../guides/formatting-and-linting/).

## `kargo add`

Add dependencies to `kargo.toml`, or enable frameworks.

```sh
kargo add <group:artifact[:version[:classifier]]>... [options]
kargo add --path <dir> [--dev] [-t <target>] [--scope <scope>]
kargo add -F <framework,...>
```

| Option | Meaning |
|---|---|
| `--path <dir>` | Depend on the Kargo package in `<dir>` (relative to the current directory), built from source. The entry is named after that package, and its path is written relative to `kargo.toml`. See [path dependencies](../../guides/workspaces/) |
| `--dev` | Add to `[dev-dependencies]` |
| `-t, --target <target>` | Add to `[target.<target>.dependencies]` |
| `--scope <scope>` | `compile`, `runtime`, `provided`, `processor` (javac annotation processor) or `ksp` (KSP processor) |
| `--bom` | Import as a BOM |
| `--pre` | Allow pre-release versions when picking the latest |
| `--no-verify` | Don't check that the artifact exists |
| `-F, --framework <list>` | Enable built-in frameworks instead |
| `-p, --package <name>` | The workspace member whose `kargo.toml` to edit |

`kargo add --path` refuses a dependency that would form a cycle, reuse another package's name or bring a second Kotlin version, and leaves `kargo.toml` unchanged.

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
| `-p, --package <name>` | The workspace member whose `kargo.toml` to edit |

Path dependencies are removed by their key: `kargo remove mathlib`. Without an option, the dependency is removed from `[dependencies]`.

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
| `-p, --package <name>` | The workspace member to check |

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
| `-p, --package <name>` | The workspace member whose `kargo.toml` to edit |

Dependencies inherited with `workspace = true` are skipped; upgrade them in the workspace's `kargo.toml`. "Compatible" follows the same rule as the `compatible` column of [`kargo outdated`](#kargo-outdated). `kargo.lock` is re-resolved on the next build because the manifest changed.

## `kargo target`

Enable, disable or list the targets of an existing project. `kargo target add` writes the same `[target.<name>]` table `kargo new` would, keeping the formatting and comments of `kargo.toml`, and scaffolds the files the target needs.

```sh
kargo target add <target>... [--lib | --app]
kargo target remove <target>...          # alias: rm
kargo target list [--format json]        # alias: ls
```

```console
$ kargo target add wasm
      Adding jvm target
note: kargo.toml had no [target] section, which means jvm only; [target.jvm] keeps the jvm target enabled
      Adding wasm target
note: created src-wasm/.gitkeep
note: run `kargo build --target wasm` to build it
```

| Option | Meaning |
|---|---|
| `<target>...` | `jvm`, `wasm`, `native` or `android`; space- or comma-separated |
| `--lib` | `add`: a Native or Android library (`kind = "library"`) |
| `--app` | `add`: a Native executable or an Android application |
| `--format json` | `list`: print JSON (`enabled` with each target's settings, and `available`) |
| `-p, --package <name>` | The workspace member whose `kargo.toml` to edit or list |

`kargo target add`:

- Writes the defaults of `kargo new`: `environment = "js"` for Wasm, `platforms = ["host"]` and a `kind` for Native, and `namespace`, `kind` and `min-sdk = 24` for Android (the namespace is the package's `group` plus its name).
- Adds `[target.jvm]` first when `kargo.toml` has no `[target]` section, because such a project is implicitly JVM and would stop building for the JVM once any other `[target.*]` table exists.
- Without `--lib` or `--app`, picks an application when the sources declare a `main` function (or the manifest sets `main-class` or a `[[bin]]`), and a library otherwise.
- Creates only missing files and never overwrites one: `src-<target>/.gitkeep` when `src-<target>/` does not exist, and for Android `android/AndroidManifest.xml` plus, for an application, a launcher activity in `src-android/MainActivity.kt`.
- Is idempotent: a target that is already enabled prints a note and leaves `kargo.toml` alone.
- Warns when an enabled framework does not support the new target (Spring Boot on Wasm, for example).

`kargo target remove` deletes `[target.<name>]` with its `dependencies` and `cinterop` sub-tables. It keeps `src-<target>/`, `tests-<target>/` (and `android/`) on disk and lists them. Removing the last enabled target is an error: add another target first.

## `kargo tree`

Show the resolved dependency graph. Conflicts are shown as `requested -> selected`; repeated subtrees are marked `(*)`.

| Option | Default | Meaning |
|---|---|---|
| `-t, --target <target>` | first enabled | Which target |
| `-c, --configuration <c>` | `runtime` | `compile`, `runtime` or `test` |
| `--depth <n>` | unlimited | Maximum depth |
| `-d, --duplicates` | | Only modules requested in several versions |
| `-p, --package <name>`, `--workspace` | current package | Which packages to show |
| `--format <fmt>` | `text` | `text` or `json`. `json` prints the whole resolved graph, with each module's version, files and direct dependencies; it can't be combined with `--depth` or `--duplicates`. When several packages are selected, the JSON output is an array with one document per package |

Path dependencies are listed first, as `mathlib v0.1.0 (<directory>)`; their Maven dependencies appear in the graph next to yours.

## `kargo classpath`

Print the resolved classpath (JVM) or klib path (Wasm, Native) of a target: the jars on disk, where `kargo tree` shows coordinates. Use it to point any tool, script or editor at a project's libraries.

```console
$ kargo classpath --format lines
/home/me/.kargo/cache/maven/io/ktor/ktor-server-core-jvm/3.2.3/ktor-server-core-jvm-3.2.3.jar
/home/me/.kargo/cache/maven/org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21.jar
...
```

The entries are the ones the build compiles with, in the same order: path dependencies first (built if needed), then the resolved Maven artifacts, including the Kotlin stdlib and what [frameworks](../../guides/frameworks/) add. The package's own classes are not included. Resolution follows `kargo.lock`, `--locked` and `--offline`, and may download dependencies and update `kargo.lock`, as `kargo tree` does.

| Option | Default | Meaning |
|---|---|---|
| `-t, --target <target>` | `jvm` | Which target. Without it, the JVM target, or the package's only Wasm or Native target. A Native target prints the klibs of its first platform; Android's classpath comes from Gradle (`kargo android -- dependencies`) |
| `-s, --scope <scope>` | `compile` | `compile` (what the main code compiles against), `runtime` (what it runs with) or `test` (what the tests compile and run against, dev-dependencies included) |
| `--format <fmt>` | `path` | `path`: one line joined with the platform's path separator (`:`, or `;` on Windows), ready for `-cp`. `lines`: one entry per line. `json`: an object with `package`, `target`, `platform`, `scope` and `classpath` |
| `-p, --package <name>` | current package | Which package |

## `kargo package`

Build a self-contained, runnable fat jar with all runtime dependencies, path dependencies included: `target/<profile>/jvm/<name>-<version>-all.jar` (`target/<profile>/<name>/jvm/` in a workspace). JVM only. The path of the jar is printed on stdout.

A library (a project without a `main` function or `main-class`) gets what you would publish instead: `target/<profile>/package/<name>-<version>.jar` with only its own classes (no Kotlin standard library, no `Class-Path`) and `<name>-<version>-sources.jar` next to it. Pass `--fat` for the fat jar anyway. `kargo publish --dry-run` shows everything a [publication](../../guides/publishing/) holds, POM included.

| Option | Meaning |
|---|---|
| `-r, --release` | Use the release profile |
| `-p, --package <name>` | The package to build the jar for; required at the root of a workspace with several members |
| `--bin <name>` | Launch this [`[[bin]]`](../manifest/#bin) instead of the main class; the jar is `<bin>-<version>-all.jar` |
| `--fat` | Build the self-contained `<name>-<version>-all.jar` even for a library |

## `kargo publish`

Build a library with the release profile and publish it to a Maven repository: the local one (`~/.m2/repository`), a [`[repositories]`](../manifest/#repositories) entry, or Maven Central. See [Publishing a library](../../guides/publishing/).

```sh
kargo publish [--repository <name> | --local] [--dry-run]
```

Every target except Android is published: a JVM-only library as one Maven module (`<name>.jar`), anything else as a multiplatform library with a root module and one module per platform (`<name>-jvm`, `<name>-wasm-js`, `<name>-linuxx64`, ...). Each module gets a POM, Gradle module metadata, a sources jar and a javadoc jar, the checksums `.md5`, `.sha1`, `.sha256` and `.sha512`, and `.asc` signatures when [signing](../../guides/publishing/#signing) is on. Everything is staged in `target/release/publish/repository` first.

| Option | Meaning |
|---|---|
| `--repository <name>` | Where to publish: a `[repositories]` entry, `maven-central` or `maven-local`. Defaults to `repository` in [`[publish]`](../manifest/#publish) |
| `--local` | Publish to the local Maven repository, like `--repository maven-local` |
| `--dry-run` | Build, stage and sign everything, but upload nothing |
| `-p, --package <name>`, `--workspace` | Which packages to publish. Path dependencies are published before the packages that use them; members with `publish = false` are skipped when the whole workspace is published |

The package needs a `group` in `[package]`. Maven Central also needs `description`, `license`, `authors` and `repository`, a release (non-SNAPSHOT) version, and signatures; Kargo checks them before building.

## `kargo fetch`

Download all dependencies and toolchains so later builds can run offline. For Android this is the [Gradle distribution](../../guides/android/#gradle) and its wrapper; Gradle fetches Android dependencies itself during the build.

| Option | Meaning |
|---|---|
| `-t, --target <target>` | Only this target (repeatable) |
| `--sources` | Also download the `-sources.jar` of every resolved library (JVM and klib targets), so editors can show and step into library code. Libraries that publish none are skipped. Sources jars are not recorded in `kargo.lock` |
| `-p, --package <name>`, `--workspace` | Which packages to fetch for |

```console
$ kargo fetch --sources
 Downloading kotlin-stdlib-2.4.21-sources.jar
     Fetched 12 sources jars
```

## `kargo update`

Re-resolve dependencies and rewrite `kargo.lock`. Cached repository metadata is ignored, so new versions within version ranges and new builds of [SNAPSHOT versions](../../guides/dependencies/#snapshot-versions) are picked up.

```sh
kargo update [group:artifact]... [--precise <version>] [--dry-run]
```

| Option | Meaning |
|---|---|
| `modules` | Only update these locked modules (direct or transitive). Every other module keeps its locked version (a SNAPSHOT, its locked build) |
| `--precise <version>` | Lock the one given module at exactly this version, for example to pick up a security fix in a transitive dependency or to step back from a broken release |
| `--dry-run` | Print the changes without writing `kargo.lock` |

Each change is printed as `Updating`, `Adding` or `Removing`:

```console
$ kargo update org.jetbrains.kotlinx:kotlinx-io-core --precise 0.7.0
    Updating org.jetbrains.kotlinx:kotlinx-io-core v0.6.0 -> v0.7.0
     Updated kargo.lock
```

In a workspace, or with path dependencies, `kargo update` re-resolves every package, since they share one `kargo.lock`. `kargo update` changes only `kargo.lock`, never `kargo.toml`; use [`kargo upgrade`](#kargo-upgrade) to change the versions you declared. A `--precise` version stays locked until `kargo.toml` changes or you run `kargo update` for that module again. See [kargo.lock](../lockfile/#commands).

## `kargo clean`

Delete everything in `target/` except the build-directory lock, `target/.kargo-lock`. In a workspace, that is the workspace's `target/`.

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

Inside a project, `kargo --help` also lists its tasks. A task named like a built-in command, such as `build` or `test`, can only run as `kargo task <name>`; the listing marks it and the commands that work on a package [warn about it](../warnings/#tasks-hidden-by-built-in-commands).

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

Manage JDKs, Kotlin/Native and Gradle distributions, and the Kotlin language servers [`kargo lsp`](#kargo-lsp) runs.

| Subcommand | Meaning |
|---|---|
| `list` (`ls`) | Installed toolchains, disk usage and what this project uses |
| `install [jdk\|native\|gradle\|kotlin-lsp\|kls\|kotlin-debug-adapter] [version]` | Install a toolchain; with no arguments, whatever the project needs. `gradle` without a version installs the one the project uses; `kotlin-lsp` and `kls` without a version install the one `kargo lsp` uses, `kotlin-debug-adapter` the one `kargo dap` uses |
| `remove` (`rm`) `<jdk\|native\|gradle\|kotlin-lsp\|kls\|kotlin-debug-adapter> <version>` | Remove one. For JDKs, a major version removes all its releases |
| `available` | JDK versions available for download (LTS marked) |
| `which` | The toolchains this project resolves to |

## `kargo daemon`

Show or stop the background compiler daemons that keep the Kotlin compiler warm between builds. See [Build performance](../../guides/build-performance/#the-compiler-daemon).

| Subcommand | Meaning |
|---|---|
| `status` | List running daemons with their process ID and state directory |
| `stop` | Stop every running daemon; the next build starts a new one |

## `kargo info`

Print project information: package, targets, path dependencies and paths. At the root of a workspace, it prints the members, the Kotlin version, the lockfile and the target directory.

| Option | Default | Meaning |
|---|---|---|
| `--format <fmt>` | `text` | `text` or `json`. `json` also lists every dependency and repository. See [Machine-readable output](../machine-readable-output/#kargo-info---format-json) |

## `kargo metadata`

Print a JSON description of the whole project for editors, language servers and other tools: every package with its source, test and resource roots, output directories, compiler arguments and plugins, entry points, tasks, and the resolved classpath (JVM) or klib path (Wasm, Native) of every target and scope. Resolving may download dependencies and update `kargo.lock`, as `kargo tree` does.

```sh
kargo metadata
kargo metadata --no-deps
kargo metadata --fingerprint
```

| Option | Meaning |
|---|---|
| `--format <fmt>` | `json`, the only format (default) |
| `--no-deps` | Leave out the resolved dependencies, plugin jars and the JDK. Nothing is downloaded or locked |
| `-r, --release` | Describe the release profile's output directories and compiler arguments |
| `--fingerprint` | Only print a hash of every `kargo.toml` and `kargo.lock`. It is the `fingerprint` of the document, and it changes when the metadata may have changed |

The output has a `"version": 1` field and is described in [Machine-readable output](../machine-readable-output/#kargo-metadata). See [Editor integration](../../guides/editors/) for how editors use it.

## `kargo lsp`

Run the language server for editors, speaking LSP over stdin and stdout. Kargo answers everything about `kargo.toml` (diagnostics, completion, hover, Maven coordinates and versions, hints at newer versions, code actions that make the edits of `kargo add`, `kargo target add` and `kargo upgrade`) and the `kargo/runnables` request itself, and passes Kotlin code to a Kotlin language server it downloads on first use and describes the project to.

```sh
kargo lsp                       # JetBrains' Kotlin LSP behind Kargo's server
kargo lsp --server kls          # fwcd's kotlin-language-server instead
kargo lsp --server none         # kargo.toml support and runnables only
```

| Option | Default | Meaning |
|---|---|---|
| `--server <name>` | `kotlin-lsp` | The Kotlin language server: `kotlin-lsp` (JetBrains), `kls` (fwcd) or `none` |
| `--server-path <path>` | | Run this server executable instead of the one Kargo downloads |
| `--klib-targets` | | Also describe Wasm and Native sources to the Kotlin LSP. The pinned Kotlin LSP can't read klibs yet, so their libraries don't resolve. See [Multiplatform projects](../../guides/editors/#multiplatform-projects) |

With `{ "checkOnSave": true }` in the editor's `initializationOptions`, saving a Kotlin or Java file runs `kargo check` in the background and adds its diagnostics for every target to the Kotlin server's; `checkTargets` limits the targets. See [Check on save](../../guides/editors/#check-on-save).

Editors that pass `--stdio` may keep it; Kargo always uses stdio. The exit code is 0 after `shutdown` and `exit`, 1 if the editor exits without `shutdown`. See [Editor integration](../../guides/editors/#language-server) for editor setups, the files it writes, the [code actions](../../guides/editors/#code-actions-in-kargotoml) and the `kargo/runnables` request.

## `kargo ide`

Write the project description a Kotlin language server reads, for editors that run JetBrains' Kotlin LSP or fwcd's kotlin-language-server themselves instead of [`kargo lsp`](#kargo-lsp). The files are the ones `kargo lsp` writes.

```sh
kargo ide           # workspace.json, for the Kotlin LSP
kargo ide --kls     # kls-classpath (kls-classpath.bat on Windows), for kotlin-language-server
kargo ide --zed     # .zed/tasks.json, for Zed's run buttons
```

| Option | Meaning |
|---|---|
| `--kls` | Write `kls-classpath` instead of `workspace.json` |
| `--klib-targets` | Also describe Wasm and Native sources in `workspace.json`. See [Multiplatform projects](../../guides/editors/#multiplatform-projects) |
| `--zed` | Write `.zed/tasks.json` instead: tasks that run `kargo run` and `kargo test` from the run buttons of Zed's Kotlin extension, the project's programs, tests and tasks, and everyday commands. Needs no dependency resolution. See [Zed](../../guides/editors/#run-and-test-in-zed) |

Kargo keeps the file current: after `kargo build`, `check`, `test`, `fetch`, `add`, `remove`, `update`, `upgrade` and `target`, it writes it again if `kargo.toml` or `kargo.lock` changed, and prints `Wrote workspace.json` (or `Wrote .zed/tasks.json`). A file Kargo didn't write is left alone, with a warning. If the dependencies can't be resolved, the project is described without them and the command fails with the reason. See [Without `kargo lsp`](../../guides/editors/#without-kargo-lsp).

## `kargo dap`

Run the debug adapter for editors: the [Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/) over stdin and stdout, in the project of the current directory.

```sh
kargo dap
```

Kargo runs [fwcd's kotlin-debug-adapter](https://github.com/fwcd/kotlin-debug-adapter) on the project's JDK, downloading it on first use like a [toolchain](../../guides/toolchains/#kotlin-language-servers), and passes the editor's messages through. It adds what the adapter lacks for a Kargo project:

| Request | What Kargo does |
|---|---|
| `launch` | Runs `kargo <args>` with `--debug` (and `-t jvm` unless `args` names a target), sends what the program prints to the editor's debug console, and attaches once the JVM waits for the debugger. A program that exits first fails the request with its exit code |
| `attach` | Attaches to a JVM started with `kargo run --debug` or `kargo test --debug` |

| Launch argument | Default | Meaning |
|---|---|---|
| `args` | `["run"]` | The arguments of `kargo`, starting with `run` or `test`: `["run", "--bin", "server"]`, `["test", "com.example.MathTest.adds"]` |
| `cwd` | the project root | The directory to run `kargo` in, relative to the project root |
| `env` | | Environment variables for `kargo` and the program |

| Attach argument | Default | Meaning |
|---|---|---|
| `port` | `5005` | The port the JVM listens on |
| `hostName` | `127.0.0.1` | Its host |

Both take `timeout` (milliseconds to try connecting, default 10000) and `logLevel` (`INFO`, `DEBUG`, `TRACE`, ...), which kotlin-debug-adapter logs to the debug console. Disconnecting stops a launched program. `--adapter-path <path>` runs another debug adapter executable instead of the one Kargo downloads. See [Debugging](../../guides/editors/#debugging).

## `kargo completions`

Print a shell completion script for `bash`, `zsh`, `fish`, `powershell` or `elvish`. It completes commands, flags and targets, plus the current project's tasks, `kargo-*` plugins, framework names and installed toolchains. See [Shell completions](../../getting-started/installation/#4-shell-completions-optional) for how to load it.

```sh
kargo completions bash
```

## `kargo <name>`

If `<name>` is a task in `kargo.toml`, run it. Otherwise run the executable `kargo-<name>` from `~/.kargo/bin` or `PATH`.
