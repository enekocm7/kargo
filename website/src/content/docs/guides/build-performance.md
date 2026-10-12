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
- It exits after 3 hours without work, and when its state directory under `~/.kargo/daemon` is removed.
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
- **Where the state lives.** The caches are in `target/<profile>/jvm/.fingerprint/<name>.ic/`, so `kargo clean` removes them.

Incremental compilation needs the compiler daemon, and a Kotlin version that publishes `kotlin-build-tools-impl` (2.4 and newer). Multiplatform projects that share code between `src/` and `src-jvm/` with `expect`/`actual`, and the Wasm and Native targets, compile each target from scratch (in the daemon for Wasm).

### Turning it off

Set `KARGO_INCREMENTAL=false` (like Cargo's `CARGO_INCREMENTAL=0`) to recompile whole targets while keeping the daemon. Kargo discards the incremental caches when it does:

```sh
KARGO_INCREMENTAL=false kargo build
```

Kargo also falls back to a full rebuild by itself when the incremental compiler fails for a reason other than a compile error, for example a corrupt cache; the details go to the daemon's log.

## Parallel work

Kargo runs independent work at the same time:

- **Targets.** `kargo build`, `kargo check` and `kargo fetch` build the JVM, Wasm, Native and Android targets of a multiplatform project concurrently.
- **Native platforms.** Each entry in `[target.native] platforms` compiles in its own Kotlin/Native process at the same time.
- **Tests.** `kargo test` compiles every target's tests concurrently, then runs them one target at a time so their output doesn't interleave.
- **Downloads.** The compiler, the JDK and your dependencies download at the same time. Dependency resolution fetches POMs and Gradle module metadata of a whole level of the graph at once, and artifacts listed in `kargo.lock` download in parallel when the cache is empty (a fresh CI machine, for example). Requests that hit a rate limit (HTTP 429) or a temporary server error are retried with backoff.

## Skipped work

- **Up-to-date checks.** Each compilation fingerprints its sources, classpath, compiler plugins and arguments. If nothing changed, `kargo build` doesn't start the compiler, and a no-op build takes milliseconds.
- **Cached toolchain lookups.** The JDK's version is read from its `release` file instead of starting `java`, and the Kotlin compiler's classpath is remembered per version, so a no-op build needs no network access.
- **Fast packaging.** `kargo package` copies the classes of dependency jars into the fat jar without decompressing and recompressing them.
- **Class-data sharing.** On JDK 19 and newer, the JUnit launcher used by `kargo test` and a standalone compiler (with the daemon off) reuse an archive of the classes they loaded on their first run, which cuts their startup time. The archives live in `~/.kargo/daemon/cds`.
