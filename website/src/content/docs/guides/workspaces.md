---
title: Workspaces and path dependencies
description: Split a project into several packages with path dependencies, and build them together in a workspace.
---

A Kargo package can depend on another package on disk, built from source, instead of a library published to a Maven repository. Several packages can also form a **workspace** that shares one `kargo.lock`, one `target/` directory, one Kotlin version and its settings, and builds its members in dependency order, in parallel.

A typical multi-module layout is a shared multiplatform core used by a JVM server, an Android app and a Native CLI:

```text
myapp/
  kargo.toml            [workspace]
  libs/core/            targets: jvm, android, wasm, native (expect/actual)
  apps/server/          jvm, depends on core
  apps/android-app/     android, depends on core
  apps/cli/             native, depends on core
```

## Path dependencies

Say `calc` is an app that uses a library `mathlib` next to it:

```text
calc/
  kargo.toml
  src/Main.kt
mathlib/
  kargo.toml
  src/Lib.kt
```

Add the dependency from `calc`:

```sh
cd calc
kargo add --path ../mathlib
```

That writes the entry with the dependency's package name as the key:

```toml title="calc/kargo.toml"
[dependencies]
mathlib = { path = "../mathlib" }
```

Now `calc` can import `mathlib`'s declarations, and `kargo build`, `run`, `test` and `package` build `mathlib` first:

```console
$ kargo run
   Compiling mathlib v0.1.0 (jvm)
   Compiling calc v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.12s
     Running `java com.example.calc.MainKt`
2 + 3 = 5
```

A path dependency is built for whichever target its consumer builds:

| Target | What the consumer gets |
|---|---|
| JVM | The dependency's jar, on the compile and runtime classpaths, in `lib/` and inside the fat jar of `kargo package` |
| Wasm | The dependency's klib |
| Native | A klib of the dependency for each platform the consumer builds, whatever the dependency's own `kind` |
| Android | An Android library (`[target.android]` with `kind = "library"`) is a Gradle subproject of the consumer's generated build; any other package is its JVM jar. See [Android members](#android-members) |

The dependency must enable every target its consumer builds. If `calc` has `[target.wasm]` and `mathlib` doesn't, the build fails and says to add `[target.wasm]` to `mathlib/kargo.toml`.

### Multiplatform packages

A multiplatform dependency keeps working across the package boundary. Its common sources (`src/`) and the sources of each target (`src-<target>/`) compile together for each target, so its `expect` declarations get their `actual` ones, and each consumer gets the artifact of its own target: a JVM `server` gets `core`'s jar, a Native `cli` gets `core`'s klib, and a Wasm app gets its Wasm klib. The common code of a consumer can use `core`'s common API, since every target it builds for sees it.

### What comes along

A path dependency brings its own Maven dependencies, including the ones its [frameworks](../frameworks/) add, so `calc` doesn't redeclare what `mathlib` needs. Path dependencies of path dependencies are followed too.

Like Gradle's `api` and `implementation`, only what a package **exports** is part of its API:

```toml title="mathlib/kargo.toml"
[dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = { version = "0.7.1", exported = true }   # in mathlib's public API
"com.squareup.okio:okio" = "3.16.0"                                               # used inside mathlib only
geometry = { path = "../geometry", exported = true }
```

- An `exported` dependency of `mathlib` is on `calc`'s compile classpath, so `calc` can use the types `mathlib`'s API exposes.
- Any other dependency of `mathlib` is only on `calc`'s runtime classpath: `mathlib` needs it to run, but `calc` can't compile against it by accident. If `calc` uses it directly, it declares it itself.
- The libraries of `mathlib`'s [frameworks](../frameworks/) (such as `kotlinx-coroutines-core`) are always exported.
- A `runtime` dependency stays `runtime`, and a `provided` one is not passed on.
- If `calc` declares `mathlib` with `scope = "runtime"`, everything `mathlib` brings is `runtime` too; with `scope = "provided"`, `calc` gets only what `mathlib` exports, as `provided`.
- `mathlib`'s `[dev-dependencies]` are only for `mathlib`'s own tests.

Wasm, JS and Native compile against klibs, which need every library they reach, so `exported` only matters for the JVM (and for Android and publishing). When `mathlib` is [published](../publishing/), its exported dependencies get the POM scope `compile`, and the others `runtime`.

If `calc` declares a module that `mathlib` also declares, `calc`'s declaration is used. `mathlib`'s `[repositories]` are searched as well, so you don't have to repeat them.

Only `scope`, `exported` and `test-fixtures` can be set on a path dependency; `version`, `exclude`, `classifier` and the other Maven keys don't apply. Path dependencies can go in `[dev-dependencies]` (only for tests) and in `[target.<target>.dependencies]` (only for one target):

```toml
[dev-dependencies]
testkit = { path = "../testkit" }

[target.jvm.dependencies]
jvm-extras = { path = "../jvm-extras", scope = "runtime" }
```

### Test fixtures

Test helpers that several packages' tests share (fakes, builders, sample data) go in a package's `test-fixtures/` directory, next to `src/` and `tests/`:

```text
mathlib/
  src/Lib.kt
  test-fixtures/Samples.kt     object Samples { val pairs = listOf(2 to 3, 4 to 5) }
  tests/LibTest.kt             uses Samples
```

Test fixtures compile against the package (including its `internal` declarations), its dependencies and its dev-dependencies. The package's own tests use them, and so do the tests of any package that asks for them with `test-fixtures = true`:

```toml title="calc/kargo.toml"
[dependencies]
mathlib = { path = "../mathlib" }

[dev-dependencies]
mathlib = { path = "../mathlib", test-fixtures = true }
```

`calc`'s tests then also get `mathlib`'s dev-dependencies, which the fixtures may use. Test fixtures work for the JVM target: `test-fixtures-jvm/` holds JVM-only ones. Wasm and Native tests that would need them stop with an error; keep fixtures that only JVM tests use in `test-fixtures-jvm/`.

### Rules

- The key must be the dependency's package name. `lib = { path = "../mathlib" }` fails if that package is called `mathlib`.
- Packages built together need unique names.
- Path dependencies can't form a cycle. A package's tests may use a package that depends on it (`[dev-dependencies]`), but nothing else may point back.
- **One Kotlin version.** Packages built together use one Kotlin compiler. When one of them sets `[kotlin] version`, the others use it too. Two different versions are an error.
- **One JDK.** Likewise, a `[toolchain] jdk` set in one package applies to all of them, and two different ones are an error.

### Where the output goes

Without a workspace, `calc` keeps its usual layout and its path dependencies build inside its `target/` directory, under `deps/`:

```text
calc/target/debug/
  jvm/calc-0.1.0.jar
  deps/mathlib/jvm/mathlib-0.1.0.jar
```

Their resolutions are locked in `calc/kargo.lock` under keys prefixed with the package name, such as `mathlib/jvm/runtime`.

## Workspaces

A workspace is a `kargo.toml` with a `[workspace]` table and no `[package]`, listing the member directories:

```text
myapp/
  kargo.toml           [workspace]
  kargo.lock           shared by every member
  target/              shared by every member
  libs/mathlib/kargo.toml
  apps/calc/kargo.toml
```

```toml title="myapp/kargo.toml"
[workspace]
members = ["libs/*", "apps/*"]

[workspace.kotlin]
version = "2.4.21"

[workspace.dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
mathlib = { path = "libs/mathlib" }
```

Members are ordinary packages. They depend on each other with path dependencies, as above.

### Creating a workspace

`kargo new --workspace` creates an empty workspace, and `kargo new` inside a workspace adds the new package to its `members`:

```console
$ kargo new myapp --workspace
     Created workspace `myapp`
$ cd myapp
$ kargo new libs/core --lib
     Created library `core` package
      Adding `libs/core` to the members of the workspace at /home/me/myapp
$ kargo new apps/server
     Created application `server` package
      Adding `apps/server` to the members of the workspace at /home/me/myapp
```

The new workspace pins the Kotlin version in `[workspace.kotlin]`. A member that a `members` glob already covers is not listed again, and members share the workspace's git repository instead of getting their own. `kargo init` in a directory inside a workspace works the same way.

### Members

`members` lists member directories, relative to the workspace's `kargo.toml`, or globs such as `libs/*`:

```toml
[workspace]
members = ["libs/*", "apps/*", "tools/codegen"]
exclude = ["libs/attic"]
default-members = ["apps/server"]
```

| Key | Meaning |
|---|---|
| `members` | Directories or globs. `*` matches any part of a name and `?` one character, within one path segment. A glob matches the directories that have a `kargo.toml`, except hidden ones, sorted by name |
| `exclude` | Directories the globs must not match |
| `default-members` | The members `kargo build` and the other commands work on at the workspace root (default: every member) |

### Shared settings

Members share the workspace's settings, and inherit what they opt in to:

| Table | How members use it |
|---|---|
| `[workspace.package]` | `version`, `group`, `description`, `authors`, `license`, `homepage` and `repository`, inherited with `version.workspace = true` |
| `[workspace.kotlin]` | `version` (the Kotlin version of every member), `language-version`, `api-version`, `ksp`, `opt-in`, `compiler-args`, `warnings-as-errors` and `jvm-target` apply to every member. A member's own `opt-in` and `compiler-args` are added to these, and its other `[kotlin]` keys (and `[target.jvm] jvm-target`) take precedence. A different `version` is an error |
| `[workspace.dependencies]` | Dependencies inherited by name with `name = { workspace = true }` |
| `[workspace.frameworks]` | Frameworks inherited with `name = { workspace = true }` in `[frameworks]` |
| `[workspace.plugins]` | Plugins inherited with `name = { workspace = true }` in `[plugins]` |
| `[workspace.tasks]` | Tasks that run at the workspace root, and that members inherit with `name = { workspace = true }` in `[tasks]` (see [Tasks](#tasks-and-plugins)) |

```toml title="myapp/kargo.toml"
[workspace]
members = ["libs/*", "apps/*"]

[workspace.package]
version = "1.2.0"
group = "com.example"
license = "Apache-2.0"

[workspace.kotlin]
version = "2.4.21"
opt-in = ["kotlin.RequiresOptIn"]
jvm-target = "17"

[workspace.dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
mathlib = { path = "libs/mathlib" }

[workspace.frameworks]
serialization = { formats = ["json"] }
```

```toml title="myapp/apps/calc/kargo.toml"
[package]
name = "calc"
version.workspace = true
group.workspace = true
license.workspace = true

[frameworks]
serialization.workspace = true

[dependencies]
mathlib = { workspace = true }
"org.jetbrains.kotlinx:kotlinx-datetime" = { workspace = true, scope = "runtime" }
```

An inherited dependency takes everything (version, exclusions, the path) from the workspace; the member may add `scope`, `exported` and `test-fixtures`. A `path` in `[workspace.dependencies]` is relative to the workspace root, and so are the `command` of a plugin and the `script` of a plugin or task in `[workspace]` tables. Any other inherited key is `{ workspace = true }` alone. `kargo upgrade` skips inherited entries; upgrade them in the workspace's `kargo.toml`.

### Choosing packages

At the workspace root, commands apply to the default members (every member, unless `default-members` says otherwise). Inside a member's directory, they apply to that member, and `kargo.lock` and `target/` are still the workspace's. Pick packages explicitly with `-p`, `--workspace` and `--exclude`:

```sh
kargo build                          # at the root: the default members
kargo build -p calc                  # calc (and what it depends on)
kargo test -p calc -p mathlib
kargo test --workspace               # every member, from anywhere in the workspace
kargo build --workspace --exclude legacy
kargo run -p server                  # run needs exactly one package
```

| Command | Package selection |
|---|---|
| `build`, `check`, `test`, `doc`, `fmt`, `lint`, `tree`, `fetch`, `publish` | `-p <name>` (repeatable), `--workspace`, `--exclude <name>` (repeatable) |
| `run`, `package`, `add`, `remove`, `outdated`, `upgrade` | `-p <name>`; at the root of a workspace, required unless there is one default member |
| `update`, `clean` | Always the whole workspace (one `kargo.lock`, one `target/`) |

`-p` also accepts a path dependency that is not a member. `kargo info` at the root lists the members and the dependency graph between them.

Kargo finds the workspace of a member by looking for a `[workspace]` in the parent directories whose `members` (and globs) cover it. A member outside the workspace directory (`members = ["../lib"]`) is only part of the workspace when you run Kargo from the workspace root, or with `--manifest-path` pointing at the workspace's `kargo.toml`; from its own directory it builds as a package of its own.

### The build graph

Members build in dependency order: a member starts once the members it depends on have finished, and independent members build at the same time. `-j` (`--jobs`), `KARGO_JOBS` or `[workspace.build] jobs` sets how many members build at once; the default is `auto`, the number of CPUs within the free memory, for `build` and `check`, and 1 for `test`, so test output doesn't interleave (see [Limiting parallelism](../build-performance/#limiting-parallelism)):

```console
$ kargo build -j 4
   Compiling core v1.2.0 (jvm)
   Compiling core v1.2.0 (wasm)
   Compiling server v1.2.0 (jvm)
   Compiling web v1.2.0 (wasm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.40s
```

After a failure, no new member starts, and the error names the member and the target:

```console
$ kargo build
error: could not build `core` (jvm)

Caused by:
  could not compile `core` due to previous errors
```

To see every broken member in one run, pass `--keep-going` (`kargo build`, `kargo check`) or `--no-fail-fast` (`kargo test`). Members that don't depend on a failed member still build, the ones that do are skipped with a note, and a summary follows with exit code 101:

```console
$ kargo build --keep-going
note: skipped `server`: a package it depends on failed
error: could not build 2 of 4 packages (core, web); 1 skipped; 1 succeeded
```

### Incremental builds across members

A member recompiles only when what it compiles against changes. When `core` changes, Kargo rebuilds `core`, and its JVM consumers recompile only if `core`'s **ABI** changed: its public, protected and `internal` declarations, their signatures and annotations, constants, and the bodies of `inline` functions (which the compiler copies into callers). A change to a private function, or to the body of a function that is not `inline`, rebuilds `core` and leaves `server` as it is; `server` runs the new code from `core`'s new jar.

The ABI of each jar is stored next to it (`core-1.2.0.jar.abi`). Wasm and Native consumers still recompile whenever a dependency's klib changes.

### Tasks and plugins

`[workspace.tasks]` are tasks that run at the workspace root:

```toml title="myapp/kargo.toml"
[workspace.tasks]
release-notes = { run = "./tools/notes.sh", description = "Write the release notes" }
codegen = "./tools/codegen.sh"
```

At the root, `kargo <task>` runs the workspace task of that name. If there is none, it runs the task of every member that defines one, in dependency order, each in its member's directory:

```console
$ kargo release-notes        # the workspace's task, at the root
$ kargo gen                  # `gen` of every member that has one
$ cd apps/server && kargo gen   # only server's `gen`
```

A member can inherit a workspace task (and the workspace tasks it `depends-on`), so it runs in the member's directory with the member's variables, for example as a hook:

```toml title="myapp/apps/server/kargo.toml"
[tasks]
codegen.workspace = true
```

`[workspace.plugins]` work the same way: a member uses one with `name = { workspace = true }` in `[plugins]`. A task or plugin written as a [Kotlin script](../tasks-and-plugins/#kotlin-scripts) keeps running the workspace's file, since its `script` stays relative to the workspace root. Tasks and hooks of a member get `KARGO_WORKSPACE_DIR`, the workspace root, besides the [usual variables](../../reference/environment/#variables-kargo-sets). `kargo task --list` at the root lists the workspace's tasks and each member's.

### Android members

An Android member can depend on path members. Kargo generates one Gradle build for the Android package, in which:

- A path dependency with `[target.android]` and `kind = "library"` is a Gradle subproject (`implementation(project(":core"))`, or `api(...)` when `exported`). It builds from its `src/`, `src-android/` and `android/` like any Android library, with its own dependencies and frameworks. An Android application can't be a dependency.
- Any other path dependency is the JVM jar Kargo builds for it (`implementation(files(...))`), with the Maven dependencies it brings, so it needs `[target.jvm]`. Keep its `jvm-target` at a level Android supports.

Several Android members (an app and the Android libraries it uses) end up in the app's generated build under `target/android/<member>/`, with each library in `modules/<name>/`, so it opens in Android Studio as one project. A library builds into its own `target/android/<library>/build`, so its AAR is where `kargo build -p <library>` puts it.

When one command builds an app and the Android libraries it uses (`kargo build` at the root), the libraries are assembled by the app's Gradle run, together with the app, so Gradle starts and configures the build once (`Compiling ui v0.1.0 (android, gradle, with app)`). The members without `[target.android]` are compiled by Kargo first, in the [build graph](#the-build-graph), as usual. One Gradle build loads one Android Gradle Plugin, so the Android packages an app uses must have the same `agp-version` (or none).

```toml title="myapp/apps/android-app/kargo.toml"
[package]
name = "android-app"
version.workspace = true

[target.android]
namespace = "com.example.app"

[dependencies]
core = { path = "../../libs/core" }
```

### Inspecting the module graph

```console
$ kargo info
workspace:    /home/me/myapp
members:      core v1.2.0, cli v1.2.0, server v1.2.0
kotlin:       2.4.21
lockfile:     /home/me/myapp/kargo.lock
target dir:   /home/me/myapp/target
graph:
    core
    cli -> core
    server -> core
kargo:        0.1.5 (abc1234, built 2026-10-31, x86_64-unknown-linux-gnu)
home:         /home/me/.kargo (1.2 GiB)
cache:        /home/me/.kargo/cache (340.2 MiB)
daemons:      none running
env:          JAVA_HOME=/usr/lib/jvm/jdk-21
```

`kargo tree` shows the path dependencies of each member as a tree before its Maven dependencies, and `kargo metadata` describes every package with its `path-dependencies` (including `exported` and `test-fixtures`), the members and the `default-members`; `kargo info --format json` at the root lists each member's `dependencies`.

`kargo publish --workspace` publishes the members in dependency order, writing each path dependency as the Maven coordinates of the package it points to. See [Publishing](../publishing/#workspaces).

### Where the output goes

Each member builds into its own directory of the shared `target/`, named after the package:

```text
myapp/target/debug/
  calc/jvm/calc-0.1.0.jar
  mathlib/jvm/mathlib-0.1.0.jar
  mathlib/jvm/mathlib-0.1.0.jar.abi
```

`kargo.lock` at the root holds every member's resolutions, under keys such as `calc/jvm/runtime`.

### Limitations

- A workspace root can't also be a package; put the root package in a member directory.
- Wasm and Native consumers recompile on every change of a dependency, not only on ABI changes.
- Test fixtures work for the JVM target only.
- A path dependency can't be an annotation or KSP processor (`scope = "processor"` or `"ksp"`). A member's own processors run only for that member.
