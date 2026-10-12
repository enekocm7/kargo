---
title: Workspaces and path dependencies
description: Split a project into several packages with path dependencies, and build them together in a workspace.
---

A Kargo package can depend on another package on disk, built from source, instead of a library published to a Maven repository. Several packages can also form a **workspace** that shares one `kargo.lock`, one `target/` directory and one Kotlin version.

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
| Android | Not supported yet; the build stops with an error |

The dependency must enable every target its consumer builds. If `calc` has `[target.wasm]` and `mathlib` doesn't, the build fails and says to add `[target.wasm]` to `mathlib/kargo.toml`.

### What comes along

A path dependency brings its own Maven dependencies, including the ones its [frameworks](../frameworks/) add, so `calc` doesn't redeclare what `mathlib` needs. Path dependencies of path dependencies are followed too. Scopes work as in Maven:

- A `compile` dependency of `mathlib` is a `compile` dependency of `calc` (a `runtime` one stays `runtime`).
- A `provided` dependency of `mathlib` is not passed on.
- If `calc` declares `mathlib` with `scope = "runtime"` or `"provided"`, everything `mathlib` brings gets that scope too.
- `mathlib`'s `[dev-dependencies]` are only for `mathlib`'s own tests.

If `calc` declares a module that `mathlib` also declares, `calc`'s declaration is used. `mathlib`'s `[repositories]` are searched as well, so you don't have to repeat them.

Only `scope` can be set on a path dependency; `version`, `exclude`, `classifier` and the other Maven keys don't apply. Path dependencies can go in `[dev-dependencies]` (only for tests) and in `[target.<target>.dependencies]` (only for one target):

```toml
[dev-dependencies]
testkit = { path = "../testkit" }

[target.jvm.dependencies]
jvm-extras = { path = "../jvm-extras", scope = "runtime" }
```

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
  mathlib/kargo.toml
  calc/kargo.toml
```

```toml title="myapp/kargo.toml"
[workspace]
members = ["mathlib", "calc"]

[workspace.kotlin]
version = "2.4.21"

[workspace.dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
mathlib = { path = "mathlib" }
```

Members are ordinary packages. They depend on each other with path dependencies, as above.

### Shared settings

| Key | Meaning |
|---|---|
| `members` | Member directories, relative to the workspace root. Each needs a `kargo.toml` with a `[package]`. Required |
| `[workspace.kotlin] version` | The Kotlin version every member builds with. A member may repeat it in `[kotlin] version`, but a different one is an error |
| `[workspace.dependencies]` | Dependencies that members inherit by name |

A member inherits an entry of `[workspace.dependencies]` with `workspace = true`, optionally with its own `scope`:

```toml title="myapp/calc/kargo.toml"
[package]
name = "calc"
version = "0.1.0"

[dependencies]
mathlib = { workspace = true }
"org.jetbrains.kotlinx:kotlinx-datetime" = { workspace = true, scope = "runtime" }
```

Everything else (version, exclusions, the path) comes from the workspace. A `path` in `[workspace.dependencies]` is relative to the workspace root. `kargo upgrade` skips inherited entries; upgrade them in the workspace's `kargo.toml`.

### Choosing packages

At the workspace root, commands apply to every member. Inside a member's directory, they apply to that member, and `kargo.lock` and `target/` are still the workspace's. Pick packages explicitly with `-p` or `--workspace`:

```sh
kargo build                    # at the root: every member
kargo build -p calc            # calc (and what it depends on)
kargo test -p calc -p mathlib
kargo test --workspace         # every member, from anywhere in the workspace
kargo run -p calc              # run needs exactly one package
```

| Command | Package selection |
|---|---|
| `build`, `check`, `test`, `doc`, `fmt`, `lint`, `tree`, `fetch` | `-p <name>` (repeatable) or `--workspace` |
| `run`, `package`, `add`, `remove`, `outdated`, `upgrade` | `-p <name>`; required at the root of a workspace with several members |
| `update`, `clean` | Always the whole workspace (one `kargo.lock`, one `target/`) |

`-p` also accepts a path dependency that is not a member. `kargo info` at the root lists the members.

Kargo finds the workspace of a member by looking for a `[workspace]` in the parent directories that lists it. A member outside the workspace directory (`members = ["../lib"]`) is only part of the workspace when you run Kargo from the workspace root, or with `--manifest-path` pointing at the workspace's `kargo.toml`; from its own directory it builds as a package of its own.

### Where the output goes

Each member builds into its own directory of the shared `target/`, named after the package:

```text
myapp/target/debug/
  calc/jvm/calc-0.1.0.jar
  mathlib/jvm/mathlib-0.1.0.jar
```

`kargo.lock` at the root holds every member's resolutions, under keys such as `calc/jvm/runtime`.

### Limitations

These are planned:

- `members` lists directories; globs (`libs/*`), `exclude` and `default-members` are not supported yet.
- A workspace root can't also be a package; put the root package in a member directory.
- Members don't inherit `[package]`, `[frameworks]` or Kotlin compiler options from the workspace.
- Members build one after another, and a change in a dependency recompiles its consumers.
- Android members can't use path dependencies.
- A path dependency can't be an annotation or KSP processor (`scope = "processor"` or `"ksp"`). A member's own processors run only for that member.
