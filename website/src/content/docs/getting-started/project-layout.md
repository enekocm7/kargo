---
title: Project layout
description: Where Kargo looks for sources, tests and resources, and where it writes output.
---

Kargo uses fixed directory names, so there is nothing to configure.

```text
my-app/
├── kargo.toml           the manifest
├── kargo.lock           pinned dependency versions and checksums (commit it)
├── src/                 common sources (.kt, plus .java on the JVM)
├── src-jvm/             JVM-only sources
├── src-wasm/            Wasm-only sources
├── src-native/          Kotlin/Native-only sources
├── src-android/         Android-only sources
├── tests/               common tests (kotlin.test)
├── tests-jvm/           JVM-only tests (likewise tests-wasm/, tests-native/, tests-android/)
├── resources/           JVM resources, put on the classpath and in the jar
├── test-resources/      resources for JVM tests
├── android/             AndroidManifest.xml, res/, assets/
└── target/              build output (git-ignored)
```

## Sources

`src/` is compiled for **every** enabled target. Put code that only makes sense on one platform in `src-<target>/`.

A common pattern is an `expect` declaration in `src/` with an `actual` for each target:

```kotlin title="src/Platform.kt"
expect fun platformName(): String
```

```kotlin title="src-jvm/Platform.jvm.kt"
actual fun platformName(): String = "JVM ${System.getProperty("java.version")}"
```

```kotlin title="src-native/Platform.native.kt"
actual fun platformName(): String = "Kotlin/Native"
```

See [Multiplatform projects](../../guides/multiplatform/) for a complete example.

Subdirectories are fine. You don't need to mirror the package name in the folder structure, but it's a good habit.

## Java sources

On the JVM, `.java` files in `src/` and `src-jvm/` are compiled together with Kotlin, so Kotlin can call Java and Java can call Kotlin.

## Output

```text
target/
├── debug/
│   ├── jvm/
│   │   ├── classes/              compiled classes
│   │   ├── lib/                  runtime dependency jars
│   │   └── my-app-0.1.0.jar
│   ├── wasm/
│   └── native/
├── release/                      same layout, built with --release
└── android/                      the generated Gradle project
```

`kargo package` writes `target/<profile>/jvm/<name>-<version>-all.jar`.

Change the output directory with the `KARGO_TARGET_DIR` environment variable.
