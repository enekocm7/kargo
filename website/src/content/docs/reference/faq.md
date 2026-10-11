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

Not yet. Each `kargo.toml` describes one package.

## Errors

### `JDK ... is too old; Kargo needs JDK 11 or newer`

Install a newer JDK, point `JAVA_HOME` at it, or add `[toolchain] jdk = "21"` and let Kargo download one.

### `could not determine the main class`

Kargo looks for a top-level Kotlin `fun main`. If there's more than one, or none, set it explicitly:

```toml
[target.jvm]
main-class = "com.example.app.MainKt"
```

The class of a top-level function in `Main.kt` is `MainKt`. If your `main` is in a Java class, give its fully qualified name, such as `com.example.app.App`. Kargo doesn't look for `main` in Java files.

### `Node.js is required to run Kotlin/Wasm programs`

Install [Node.js](https://nodejs.org) and make sure `node` is on your `PATH`. Building Wasm doesn't need it; running and testing do.

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

### A dependency has the wrong version

Run `kargo tree --duplicates` to see who asks for what. The highest requested version wins; add `force = true` to your own declaration to pin one.

### The compiler runs out of memory

```sh
export KARGO_COMPILER_JVM_OPTS="-Xmx4g"
```

### Something is cached wrong

```sh
kargo clean                 # remove build output
kargo build --refresh       # re-check repositories, ignoring cached metadata and kargo.lock
```

Use `-v` to see the exact commands Kargo runs.
