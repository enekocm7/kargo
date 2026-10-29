---
title: Build performance
description: How Kargo keeps builds fast (the compiler daemon, incremental compilation, parallel targets and downloads) and how to tune or turn it off.
---

Most of the time of a small Kotlin build is not compiling: it's starting a JVM and warming up the compiler's JIT. Kargo avoids that cost, recompiles only the files affected by a change, builds independent work at the same time, and skips work whose inputs haven't changed.

```console
$ touch src/Main.kt && kargo build
   Compiling app v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.41s
```

## The compiler daemon

The first compilation starts a **compiler daemon**: a background JVM that runs the Kotlin compiler (and `javac`) in-process. Later builds send their compiler arguments to it over a local socket, so they get an already-running, already-optimized compiler. A rebuild of a small project drops from several seconds to well under one.

- One daemon runs per JDK, Kotlin version and `KARGO_COMPILER_JVM_OPTS`; changing any of them starts a new one.
- If you press Ctrl-C during a build, the daemon finishes that compilation without replying. A build you start right after waits for it, because the daemon runs one compilation at a time per output directory and per set of incremental caches, so the two never write into the same classes directory or caches. The same goes for an editor's check on save that is cancelled by the next save. Paths that differ only in spelling, such as `F:\` and `f:\` on Windows, count as the same directory.
- It exits after 3 hours without work, and when its state directory under `~/.kargo/daemon` is removed. On exit it deletes its `info` file. If one is left behind (after a crash or a reboot) and another program now uses its port, Kargo notices that the program does not answer like the daemon within 10 seconds and starts a new daemon instead of waiting.
- It serves several builds at once, so parallel targets and concurrent `kargo` invocations share it.
- Compiler output looks exactly like a standalone compiler run, with paths relative to the directory you ran `kargo` in.

The first build after a reboot or a Kotlin upgrade pays the JVM startup once. That build also compiles the small daemon program with your JDK's `javac` (once per JDK), and Kargo records the classes the JVM loads at startup so later daemon starts are faster.

### See and stop daemons

```console
$ kargo daemon status
pid 48211    /home/me/.kargo/daemon/ada07be3e284d798
$ kargo daemon stop
     Stopped 1 compiler daemon
```

Stop the daemons after changing your JDK installation in place, or to free their memory right away.

### Memory

The daemon uses the JVM's default heap limit (a quarter of physical memory). Raise or lower it with `KARGO_COMPILER_JVM_OPTS`, which starts a daemon with the new options on the next build:

```sh
export KARGO_COMPILER_JVM_OPTS="-Xmx4g"
```

### Turning it off

Set `KARGO_DAEMON=false` to run the compiler as a separate process on every build, as Kargo did before the daemon existed:

```sh
KARGO_DAEMON=false kargo build
```

Kargo also falls back to a separate process on its own when the daemon can't be used, for example when the JDK has no `javac` to build it. Run with `-v` to see why.

:::caution
The daemon's working directory is not your project. Relative paths in `[kotlin] compiler-args` or `[profile.*] compiler-args` resolve against the daemon's directory, so use absolute paths there (or turn the daemon off).
:::

## Incremental compilation

On the JVM, Kargo recompiles only the files you changed and the files that depend on what changed, not the whole target. It uses the Kotlin Build Tools API, the same incremental compiler Gradle uses, inside the compiler daemon.

```console
$ echo "// tweak" >> src/gen/G5.kt && kargo build
   Compiling bigproj v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.39s
```

In a project with 300 source files, changing one file rebuilds in about 0.4 seconds instead of about 5 seconds (with the daemon) or more than 20 seconds (without it).

- **What triggers a recompile.** Kargo tells the compiler which sources changed since the last successful build. The compiler also recompiles the files that use a declaration whose signature changed, and the users of a `const val` whose value changed.
- **Dependencies.** Each classpath jar is summarized in a snapshot (cached under `~/.kargo/daemon/ic-snapshots`), so adding, removing or upgrading a dependency recompiles only the code that uses what changed. The tests' snapshot of the main classes works the same way, so a change in `src/` recompiles only the tests that depend on it.
- **Clean rebuilds.** Changing compiler options, compiler plugins, `[frameworks]` or the Kotlin version recompiles the target from scratch once. A clean build takes somewhat longer than without incremental compilation, because the compiler also records what each file depends on.
- **Deleted files.** Their classes are removed from `target/` on the next successful build.
- **Java sources.** Kotlin compiles incrementally; `javac` still compiles all Java sources of a target whenever it rebuilds.
- **Running programs.** `kargo run --watch` builds this way on every change and restarts the program. `kargo run --hot` also keeps it running: the changed classes are swapped into the live JVM, so a method-body change reaches the running program in about the time of the incremental build. See [Hot reload](../watch/#hot-reload-a-jvm-program).
- **Where the state lives.** The caches are in `target/<profile>/jvm/.fingerprint/<name>.ic/`, so `kargo clean` removes them.

- **Path dependencies.** A [path dependency](../workspaces/)'s jar counts by its ABI: its public declarations, their signatures and constants, and the bodies of `inline` functions. A change to a private function, or to the body of a function that isn't `inline`, rebuilds the dependency but none of its JVM consumers, which run the new code from its new jar. See [Incremental builds across members](../workspaces/#incremental-builds-across-members).

Incremental compilation needs the compiler daemon, and a Kotlin version that publishes `kotlin-build-tools-impl` (2.4 and newer). Multiplatform projects that share code between `src/` and `src-jvm/` with `expect`/`actual`, and the Wasm, JS and Native targets, compile each target from scratch (in the daemon for Wasm and JS).

### Turning it off

Set `KARGO_INCREMENTAL=false` (like Cargo's `CARGO_INCREMENTAL=0`) to recompile whole targets while keeping the daemon. Kargo discards the incremental caches when it does:

```sh
KARGO_INCREMENTAL=false kargo build
```

Kargo also falls back to a full rebuild by itself when the incremental compiler fails for a reason other than a compile error, for example a corrupt cache. The details go to the daemon's log, not to the build output or your editor's diagnostics. If the failure leaves caches open inside the daemon, the daemon exits once idle and the next build starts a fresh one, so later builds are incremental again.

## Parallel work

Kargo runs independent work at the same time:

- **Targets.** `kargo build`, `kargo check` and `kargo fetch` build the JVM, Wasm, JS, Native and Android targets of a multiplatform project concurrently.
- **Workspace members.** `kargo build` and `kargo check` build independent [workspace](../workspaces/#the-build-graph) members at the same time, each after the members it depends on. `kargo test`, `bench`, `doc`, `fmt` and `lint` handle one member at a time unless you set a number of jobs.
- **Native platforms.** Each entry in `[target.native] platforms` compiles in its own Kotlin/Native process at the same time.
- **Tests.** `kargo test` compiles every target's tests concurrently, then runs them one target at a time so their output doesn't interleave.
- **Downloads.** The compiler, the JDK and your dependencies download at the same time. Dependency resolution fetches POMs and Gradle module metadata of a whole level of the graph at once, and artifacts listed in `kargo.lock` download in parallel when the cache is empty (a fresh CI machine, for example). Requests that hit a rate limit (HTTP 429) or a temporary server error are retried with backoff.

### Limiting parallelism

The number of jobs limits the members, and the targets and Native platforms of each member, that build at the same time. Each job can start a compiler, a Kotlin/Native link or a test JVM, so a lower number uses less memory, and a higher one keeps more cores busy:

```sh
kargo build -j 2
```

| Where | Example | Applies to |
|---|---|---|
| `-j, --jobs <n>` | `kargo build -j 2` | This command (`build`, `check`, `test`, `bench`, `run`, `package`, `doc`, `fmt`, `lint`) |
| `KARGO_JOBS` | `KARGO_JOBS=2 kargo test` | Every command in this shell |
| `[build] jobs` in `kargo.toml` | `jobs = 2` | The project; in a workspace, write it as `[workspace.build]` in the root `kargo.toml` |
| `[build] jobs` in [`config.toml`](../offline-and-mirrors/#configuration-files) | `jobs = 2` | Every project on this machine (`~/.kargo/config.toml`) or this checkout (`.kargo/config.toml`) |

The first one set wins, in the order of the table. Each takes a number, or `auto` (also written `0`), which is also the default: the number of CPUs, lowered so that every job has 1.5 GiB of the memory that is free when the command starts (on Linux within the container's cgroup limit; on macOS half of the physical memory counts as free). A 4-core CI runner with 3 GiB free gets 2 jobs.

`-j 1` builds one member, then one target, at a time, always in the same order, so the status lines of a log come out the same on every run. `kargo test` runs one test JVM per member at a time, so `-j` also sets how many test JVMs run at once; without a number of jobs it tests one member at a time. `kargo -v build` prints the numbers it uses.

The Kotlin compiler daemon is one JVM that all jobs share, so more jobs add requests to it, not JVMs. It runs with the JVM's default heap limit, a quarter of the physical memory; set another with [`KARGO_COMPILER_JVM_OPTS`](../../reference/environment/), such as `KARGO_COMPILER_JVM_OPTS=-Xmx2g`, and stop the running daemon with `kargo daemon stop` so the next build starts one with it.

## Skipped work

- **Up-to-date checks.** Each compilation fingerprints its sources, classpath, compiler plugins and arguments. If nothing changed, `kargo build` doesn't start the compiler, and a no-op build takes milliseconds.
- **Cached toolchain lookups.** The JDK's version is read from its `release` file instead of starting `java`, and the Kotlin compiler's classpath is remembered per version, so a no-op build needs no network access.
- **Fast packaging.** `kargo package` copies the classes of dependency jars into the fat jar without decompressing and recompressing them.
- **Kotlin/Native standard library.** Debug builds of a native target link a precompiled copy of the standard library instead of lowering it again, which roughly halves a rebuild. The compiler builds it on the first debug build (one to two minutes, once per Kotlin version and platform); `--release` builds don't use it. See [Build speed](../native/#build-speed) for the details and `KARGO_NATIVE_CACHE=false`.
- **Class-data sharing.** On JDK 19 and newer, the JUnit launcher used by `kargo test` and a standalone compiler (with the daemon off) reuse an archive of the classes they loaded on their first run, which cuts their startup time. The archives live in `~/.kargo/daemon/cds`.
