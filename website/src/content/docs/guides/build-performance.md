---
title: Build performance
description: How Kargo keeps builds fast (the compiler daemon, parallel targets and downloads) and how to tune or turn it off.
---

Most of the time of a small Kotlin build is not compiling: it's starting a JVM and warming up the compiler's JIT. Kargo avoids that cost, builds independent work at the same time, and skips work whose inputs haven't changed.

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

:::note
Kargo doesn't compile incrementally yet: when a source file of a target changes, the whole target (main or tests) is recompiled. The daemon makes that recompilation fast.
:::
