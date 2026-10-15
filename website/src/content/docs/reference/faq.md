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

IntelliJ IDEA and Android Studio are the best supported editors for Kotlin. Their [Kargo plugin](../../guides/editors/#intellij-idea-and-android-studio) imports the project from `kargo metadata`, so completion and navigation know your dependencies. Zed has an [extension](../../guides/editors/#zed), and other editors can use [`kargo lsp`](../../guides/editors/), which serves Kotlin with [Kargo's own Kotlin server](../../guides/kotlin-server/). For Android, you can open the generated `target/android` project in Android Studio.

### Completion in my editor doesn't know a library's classes

[Kargo's Kotlin server](../../guides/kotlin-server/#libraries), the default of `kargo lsp`, reads libraries from their sources jars. Run `kargo fetch --sources` and restart the editor's language server. Java classes, including the JDK's, and libraries that publish no sources jar stay unknown to it. For them, use `kargo lsp --server kotlin-lsp`, which reads compiled classes, or IntelliJ IDEA.

### Can I use Java?

Yes, on the JVM and Android. `.java` files next to your Kotlin files are compiled together with them, and Kotlin and Java can call each other. Java isn't compiled for Wasm or Native. See [Java interop](../../getting-started/project-layout/#java-interop).

### Are multi-module projects supported?

Yes. A package can depend on another package on disk with `{ path = "../lib" }`, and a `[workspace]` builds several packages with one `kargo.lock` and one `target/`. See [Workspaces and path dependencies](../../guides/workspaces/). Path dependencies don't work for the Android target yet.

## Errors

### `JDK ... is too old; Kargo needs JDK 11 or newer`

Install a newer JDK, point `JAVA_HOME` at it, or add `[toolchain] jdk = "21"` and let Kargo download one.

### `JDK ... is too old; Kargo needs JDK 17 or newer` when running tests

`kargo test` runs the JUnit 6 launcher on the project's JDK, and JUnit 6 needs JDK 17 or newer. Install a newer JDK, point `JAVA_HOME` at it, or add `[toolchain] jdk = "21"` and let Kargo download one. See [Testing](../../guides/testing/#what-runs-where).

### `Kotlin ... cannot run on JDK ...`

Compilers older than Kotlin 2.1.10 can't run on JDK 25 or newer. Use Kotlin 2.1.10 or newer, or add `[toolchain] jdk = "21"` so Kargo builds with JDK 21. See [Older Kotlin versions on new JDKs](../../guides/toolchains/#older-kotlin-versions-on-new-jdks).

### `jvm-target ... is newer than JDK ...` or `invalid [target.jvm] jvm-target`

`[target.jvm] jvm-target` is the Java version of the bytecode Kargo produces, and the JDK that builds the project can't target a version newer than itself. Lower `jvm-target`, or add `[toolchain] jdk = "26"` (the same version) so Kargo downloads and builds with a JDK that can. The default target never triggers this: it is lowered to your JDK's version.

`invalid [target.jvm] jvm-target` means the value isn't a Java version: use `8` (or `1.8`), or a number from `9` up such as `17` or `21`. See [`[target.jvm]`](../manifest/#targetjvm).

### `could not determine the main class`

Kargo looks for a top-level Kotlin `fun main`, or a `@JvmStatic fun main` in an `object` or `companion object`. The message says why detection failed: ``no `main` function found``, or ``found multiple `main` functions`` followed by the classes it found. Set the main class explicitly:

```toml
[target.jvm]
main-class = "com.example.app.MainKt"
```

The class of a top-level function in `Main.kt` is `MainKt`; for `object Cli { @JvmStatic fun main(...) }` it is `Cli`. If your `main` is in a Java class, give its fully qualified name, such as `com.example.app.App`. Kargo doesn't look for `main` in Java files.

If the project really has several programs, keep them all: run one with `kargo run --main com.example.tools.MigrateKt`, or declare each as a `[[bin]]` and use `kargo run --bin <name>`. See [Several programs in one project](../../guides/multiple-programs/).

### `no Kotlin or Java sources found in src/ ...`

`kargo build` has nothing to compile for that target, so it succeeds without doing anything; `kargo run` then fails later with `could not determine the main class`. Put your code in `src/` (or `src-<target>/` for one target) and make sure the files end in `.kt` (or `.java` on the JVM). If a [`pre-build` hook](../../guides/tasks-and-plugins/#hooks) generates the sources, the warning goes away once the hook has run. See [Warnings](../warnings/#no-sources).

### A `.kts` script does nothing, or a task named `build` doesn't run

Kargo doesn't compile or run Kotlin scripts, and `kargo build` always runs the built-in command, not a task of that name. Both print a warning that says what to do instead: put the code in a `.kt` file, and run the task with `kargo task build` or rename it. See [Warnings](../warnings/).

### `cannot find symbol` for a generated class such as `AutoValue_Person`

The annotation processor didn't run. Kargo only runs processors you declare with `scope = "processor"`; a processor that is merely on the classpath is ignored (JDK 23 and newer ignore it too). Move the processor artifact to `scope = "processor"`, keep its annotations as a `provided` dependency, and see [Annotation processors](../../guides/dependencies/#annotation-processors).

### `unresolved reference` for a class a KSP processor generates, such as `PersonJsonAdapter`

The KSP processor didn't run. Declare it with `scope = "ksp"`, not `processor` (which only runs `javac` processors over Java sources), and see [Kotlin Symbol Processing](../../guides/dependencies/#kotlin-symbol-processing-ksp). With `-v`, Kargo prints the arguments it passes to KSP; the generated files are under `target/<profile>/jvm/generated/ksp/`.

### `Node.js is required to run Kotlin/Wasm programs`

Install [Node.js](https://nodejs.org) and make sure `node` is on your `PATH`. Building Wasm doesn't need it; running and testing do.

### `your Wasm dependencies need the npm packages ...`

A Wasm dependency, such as `kotlinx-datetime`, imports npm packages at runtime, and Kargo installs them with npm, pnpm or bun. Install [Node.js](https://nodejs.org), which includes npm, or install [pnpm](https://pnpm.io) or [bun](https://bun.sh), and make sure it is on your `PATH`. If you set `package-manager` in `[target.wasm]`, that tool must be on your `PATH`. See [npm packages](../../guides/wasm/#npm-packages).

### `no browser found to run the Wasm tests`

A Wasm project with `environment = "browser"` runs its tests in a browser without a window, and Kargo found none. Install Chrome, Chromium, Edge, Brave or Firefox, or set `KARGO_BROWSER` to the path of one. In a container, also set `KARGO_BROWSER_ARGS=--no-sandbox` if Chrome refuses to start as root. See [Testing in a browser](../../guides/wasm/#testing-in-a-browser).

### Wasm browser tests fail with `timed out ... without output`

The browser started but its page stopped reporting, so Kargo gave up after `KARGO_BROWSER_TIMEOUT` seconds (default 120). A test that runs longer than that without printing needs a higher value. If nothing ever ran, the browser may be too old for WebAssembly garbage collection and exception handling; update it.

### How do I run my Wasm code in a browser?

Set `environment = "browser"` in `[target.wasm]` and run `kargo run -t wasm`. Kargo writes a page, serves it on `http://127.0.0.1:8080/` and, with `--open`, opens it. See [Browser apps](../../guides/wasm/#browser-apps).

### How do I publish a Kotlin/Wasm library to npm?

Set `kind = "library"` in `[target.wasm]`, mark the functions to expose with `@JsExport`, build, and run `npm publish target/release/wasm`. See [Libraries and npm packages](../../guides/wasm/#libraries-and-npm-packages).

### `Cannot find package '...'` when running Wasm

Node.js can't find an npm package that a library imports. Kargo installs these packages into `target/<profile>/node_modules` when it builds. If you deleted part of that directory, run `kargo clean` and build again.

### `Android SDK not found; set ANDROID_HOME`

Install the Android SDK (Android Studio installs it to `~/Android/Sdk` on Linux and `~/Library/Android/sdk` on macOS) and set `ANDROID_HOME`.

### `Gradle ... is too old for Android Gradle Plugin ...`

The `gradle-version` under `[target.android]` is older than the `agp-version` needs. Use the version the message names, or remove `gradle-version` to let Kargo pick it. See [Android](../../guides/android/#gradle).

### `Gradle ... is not installed and Kargo is offline`

Kargo downloads Gradle for Android builds on first use. Run `kargo fetch` (or `kargo toolchain install gradle`) once while online.

### `... is not in the local cache and Kargo is running in offline mode`

`--offline` (or `KARGO_OFFLINE`) allows only what is already downloaded. Run `kargo fetch` once while online: it downloads the dependencies, compilers and test tools the project needs, so the offline build works. Add `--tools` for the doc, fmt and lint tools.

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
