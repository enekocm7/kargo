---
title: FAQ & troubleshooting
description: Answers to common questions and fixes for common errors.
---

## General

### Does Kargo replace Gradle?

For JVM, Wasm and Native projects, yes: Kargo calls the Kotlin compilers directly and Gradle is not involved. Android builds still use the Android Gradle Plugin, but Kargo generates and runs that Gradle project for you.

### Can I use any Maven library?

Yes. Anything on Maven Central or in a repository you add in `[repositories]`, including Kotlin Multiplatform libraries.

### Does my IDE understand Kargo projects?

There's no IDE plugin yet. IntelliJ IDEA and VS Code still highlight Kotlin files, but dependency-aware completion needs a build model the IDE understands. For Android, you can open the generated `target/android` project in Android Studio.

### Can I use Java?

Yes, on the JVM and Android. `.java` files next to your Kotlin files are compiled together with them, and Kotlin and Java can call each other. Java isn't compiled for Wasm or Native. See [Java interop](../../getting-started/project-layout/#java-interop).

### Are multi-module projects supported?

Yes. A package can depend on another package on disk with `{ path = "../lib" }`, and a `[workspace]` builds several packages with one `kargo.lock` and one `target/`. See [Workspaces and path dependencies](../../guides/workspaces/). Path dependencies don't work for the Android target yet.

## Errors

### `JDK ... is too old; Kargo needs JDK 11 or newer`

Install a newer JDK, point `JAVA_HOME` at it, or add `[toolchain] jdk = "21"` and let Kargo download one.

### `Kotlin ... cannot run on JDK ...`

Compilers older than Kotlin 2.1.10 can't run on JDK 25 or newer. Use Kotlin 2.1.10 or newer, or add `[toolchain] jdk = "21"` so Kargo builds with JDK 21. See [Older Kotlin versions on new JDKs](../../guides/toolchains/#older-kotlin-versions-on-new-jdks).

### `could not determine the main class`

Kargo looks for a top-level Kotlin `fun main`, or a `@JvmStatic fun main` in an `object` or `companion object`. The message says why detection failed: ``no `main` function found``, or ``found multiple `main` functions`` followed by the classes it found. Set the main class explicitly:

```toml
[target.jvm]
main-class = "com.example.app.MainKt"
```

The class of a top-level function in `Main.kt` is `MainKt`; for `object Cli { @JvmStatic fun main(...) }` it is `Cli`. If your `main` is in a Java class, give its fully qualified name, such as `com.example.app.App`. Kargo doesn't look for `main` in Java files.

If the project really has several programs, keep them all: run one with `kargo run --main com.example.tools.MigrateKt`, or declare each as a `[[bin]]` and use `kargo run --bin <name>`. See [Several programs in one project](../../guides/multiple-programs/).

### `cannot find symbol` for a generated class such as `AutoValue_Person`

The annotation processor didn't run. Kargo only runs processors you declare with `scope = "processor"`; a processor that is merely on the classpath is ignored (JDK 23 and newer ignore it too). Move the processor artifact to `scope = "processor"`, keep its annotations as a `provided` dependency, and see [Annotation processors](../../guides/dependencies/#annotation-processors).

### `unresolved reference` for a class a KSP processor generates, such as `PersonJsonAdapter`

The KSP processor didn't run. Declare it with `scope = "ksp"`, not `processor` (which only runs `javac` processors over Java sources), and see [Kotlin Symbol Processing](../../guides/dependencies/#kotlin-symbol-processing-ksp). With `-v`, Kargo prints the arguments it passes to KSP; the generated files are under `target/<profile>/jvm/generated/ksp/`.

### `Node.js is required to run Kotlin/Wasm programs`

Install [Node.js](https://nodejs.org) and make sure `node` is on your `PATH`. Building Wasm doesn't need it; running and testing do.

### `your Wasm dependencies need the npm packages ...`

A Wasm dependency, such as `kotlinx-datetime`, imports npm packages at runtime, and Kargo installs them with npm, pnpm or bun. Install [Node.js](https://nodejs.org), which includes npm, or install [pnpm](https://pnpm.io) or [bun](https://bun.sh), and make sure it is on your `PATH`. If you set `package-manager` in `[target.wasm]`, that tool must be on your `PATH`. See [npm packages](../../guides/wasm/#npm-packages).

### `Cannot find package '...'` when running Wasm

Node.js can't find an npm package that a library imports. Kargo installs these packages into `target/<profile>/node_modules` when it builds. If you deleted part of that directory, run `kargo clean` and build again.

### `Android SDK not found; set ANDROID_HOME`

Install the Android SDK (Android Studio installs it to `~/Android/Sdk` on Linux and `~/Library/Android/sdk` on macOS) and set `ANDROID_HOME`.

### `Gradle is required for Android builds`

Install Gradle, or put a `gradlew` wrapper in the project root.

### `kargo.lock needs to be updated ... but --locked was passed`

You changed `kargo.toml` without committing the updated lockfile. Run `kargo build` locally and commit `kargo.lock`.

### `the native target has no build for this host`

Add `"host"` to `[target.native] platforms` to run locally.

### `no JVM tests found` or `no JVM tests match ...`

`kargo test` compiled your JVM tests but the JUnit Platform found nothing to run. Without a filter, check that test functions carry `@Test` (from `kotlin.test` or JUnit) and that the engine of any other framework (for example `io.kotest:kotest-runner-junit5`) is in `[dev-dependencies]`. With a filter, remember that it matches `package.Class.method` names; see [Filtering tests](../../guides/testing/#filtering-tests).

### A task prints `$KARGO_PKG_NAME` literally on Windows, or a plugin fails with `not a valid Win32 application`

Tasks run in `cmd` on Windows, which doesn't understand `$VAR`, `mkdir -p` or `./script.sh`. Add `shell = "sh"` to the task to run it in Git for Windows' `sh` everywhere, or give Windows its own commands with `windows-run`. A hook plugin's `.sh` script runs through `sh` automatically; for any other non-executable `command`, set `windows-command`. See [Tasks and plugins](../../guides/tasks-and-plugins/#windows).

### A dependency has the wrong version

Run `kargo tree --duplicates` to see who asks for what. The highest requested version wins; add `force = true` to your own declaration to pin one.

### A SNAPSHOT dependency isn't found, or doesn't update

Add the repository that holds the SNAPSHOT to `[repositories]`; Maven Central doesn't accept them. Kargo finds timestamped builds (`lib-1.0-20240101.120000-1.jar`) through the `1.0-SNAPSHOT/maven-metadata.xml` in that repository. A build stays locked in `kargo.lock`, so to get a newer one run `kargo update`. If it fails with `could not find`, the repository probably deleted the locked build; `kargo update` locks the current one. See [SNAPSHOT versions](../../guides/dependencies/#snapshot-versions).

### The compiler runs out of memory

```sh
export KARGO_COMPILER_JVM_OPTS="-Xmx4g"
```

The next build starts a compiler daemon with the new options. See [Build performance](../../guides/build-performance/#memory).

### A build after an edit behaves as if the old code were still there

Incremental compilation should never produce stale classes; if it does, please report it. To recover, run `kargo clean` (or build once with `KARGO_INCREMENTAL=false`), which discards the incremental caches.

### The compiler daemon misbehaves

Run `kargo daemon stop`; the next build starts a fresh daemon. To rule the daemon out, build with `KARGO_DAEMON=false`, which runs the compiler as a separate process. `-v` shows whether a build used the daemon, and its log is `daemon.log` in the directory `kargo daemon status` prints.

### Something is cached wrong

```sh
kargo clean                 # remove build output
kargo build --refresh       # re-check repositories, ignoring cached metadata and kargo.lock
```

`--refresh` also re-verifies cached artifacts against the repository's checksums and downloads corrupt ones again. Use `-v` to see the exact commands Kargo runs.

### `Blocking waiting for file lock on build directory`

Another Kargo process is building or resolving the same project, for example an IDE, a file watcher or a second terminal. The command continues once the other process finishes. If nothing else is running, check for a hung `kargo` process (a stuck hook, for example) and stop it. The operating system releases the lock when that process exits. See [Concurrent commands](../commands/#concurrent-commands).

### "checksum mismatch" for a locked artifact

A re-downloaded file doesn't match the SHA-256 in `kargo.lock`. Read the rest of the message: if the download matches the checksum the repository publishes, the lockfile entry is the suspect and `kargo update` re-locks it. If the repository publishes no checksum, find out why the artifact changed before you re-lock. See [Checksum mismatches](../lockfile/#checksum-mismatches).
