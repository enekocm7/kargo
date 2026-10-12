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
├── resources/           JVM resources, put on the classpath and in the jar; also the files of a Wasm browser app
├── test-resources/      resources for JVM tests
├── android/             AndroidManifest.xml, res/, assets/
├── interop/             cinterop .def files for C libraries (Kotlin/Native)
├── examples/            example programs, run with kargo run --example <name>
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

Kargo compiles `.kt` files (and `.java` files on the JVM). Kotlin scripts (`.kts`) are not compiled or run, and a target with no sources has nothing to build; `kargo build` prints a [warning](../../reference/warnings/) for both instead of staying silent.

## Several programs

A project can hold more than one `fun main`. Run any of them with `kargo run --main <class>`, give them names with [`[[bin]]`](../../reference/manifest/#bin) tables, and keep example programs in `examples/<name>.kt` for `kargo run --example <name>`. See [Several programs in one project](../../guides/multiple-programs/).

## Java interop

On the JVM, `.java` files in `src/` and `src-jvm/` are compiled together with Kotlin. Kotlin can call Java and Java can call Kotlin, and there is nothing to configure.

```java title="src/com/example/app/Greeter.java"
package com.example.app;

public class Greeter {
    public static String greet(String name) {
        return "Hello from Java, " + name + "!";
    }
}
```

```kotlin title="src/Main.kt"
package com.example.app

fun main() {
    println(Greeter.greet("Kotlin"))
}
```

Kargo runs the Kotlin compiler first, with the Java sources on its input so Kotlin can see them. Then it runs `javac` on the Java files with the Kotlin output and your dependencies on the classpath. `javac` compiles with `--release` set to your `jvm-target`, so Java and Kotlin always agree on the bytecode level.

### Java entry points

Kargo only looks for a Kotlin `fun main` (top-level, or `@JvmStatic` in an object or companion object). If your `main` is in a Java class, name it in the manifest, otherwise `kargo run` stops with `could not determine the main class`:

```toml title="kargo.toml"
[target.jvm]
main-class = "com.example.app.App"
```

### Java tests

`.java` files in `tests/` and `tests-jvm/` are compiled and run on the JUnit Platform, so JUnit 5 tests work as they are:

```java title="tests/GreeterTest.java"
import com.example.app.Greeter;
import org.junit.jupiter.api.Test;
import static org.junit.jupiter.api.Assertions.assertEquals;

class GreeterTest {
    @Test
    void greets() {
        assertEquals("Hello from Java, Kargo!", Greeter.greet("Kargo"));
    }
}
```

### Good to know

- **A full JDK is required.** If the JDK has no `javac` (a JRE-only install, for example), the build fails and tells you to install a full JDK. Pin one with `[toolchain] jdk = "21"` and Kargo downloads a Temurin JDK, which includes `javac`.
- **Java is for the JVM and Android.** The Wasm and Native targets only compile `.kt` files and ignore `.java` files. In a multiplatform project, keep Java in `src-jvm/` so that common code in `src/` doesn't depend on it. On Android, the generated Gradle project compiles the Java files in `src/` and `src-android/`.
- **Annotation processing is off by default.** `javac` runs with `-proc:none`. Declare a processor with `scope = "processor"` (see [Annotation processors](../../guides/dependencies/#annotation-processors)) or turn on the `lombok` [framework](../../guides/frameworks/), and Kargo runs it for the main and test sources. These processors only see Java sources; for processors that read Kotlin code, use [KSP](../../guides/dependencies/#kotlin-symbol-processing-ksp) (`scope = "ksp"`).
- **Extra `javac` options** go in `[target.jvm] javac-args`, e.g. `javac-args = ["-Xlint:all"]`. The `--release` flag comes from `jvm-target`.
- **Java-only projects work.** The Kotlin compiler step is skipped when there are no `.kt` files. But `kargo new` always creates Kotlin templates, and you must set `main-class` yourself.
- **Compile errors** come straight from `javac`, followed by `could not compile Java sources of ... due to previous errors`.

## Output

```text
target/
├── debug/
│   ├── jvm/
│   │   ├── classes/              compiled classes
│   │   ├── lib/                  runtime dependency jars
│   │   ├── examples/             compiled examples, one directory each
│   │   └── my-app-0.1.0.jar
│   ├── wasm/                     the module (.mjs, .wasm); for a browser app also index.html and resources/, for a library package.json and .d.mts
│   ├── node_modules/             npm packages of Wasm dependencies, if any
│   ├── native/                   one directory per platform; cinterop/ holds C bindings, bin/ the [[bin]] executables
│   └── doc/                      API documentation from kargo doc
├── release/                      same layout, built with --release
├── lint/                         Kargo's detekt settings for kargo lint
├── android/                      the generated Gradle project
└── .kargo-lock                   build-directory lock (see below)
```

`kargo package` writes `target/<profile>/jvm/<name>-<version>-all.jar` (`<bin>-<version>-all.jar` with `--bin`).

[Path dependencies](../../guides/workspaces/) build into `target/<profile>/deps/<name>/`. In a workspace, the root has the only `kargo.lock` and `target/`, and each member builds into `target/<profile>/<name>/` (its `lint/` and `android/` directories get a `<name>/` level too):

```text
myapp/
├── kargo.toml                    [workspace] with members = ["calc", "mathlib"]
├── kargo.lock
├── calc/                         a member: kargo.toml, src/, tests/, ...
├── mathlib/
└── target/
    └── debug/
        ├── calc/jvm/calc-0.1.0.jar
        └── mathlib/jvm/mathlib-0.1.0.jar
```

Change the output directory with the `KARGO_TARGET_DIR` environment variable.

`target/.kargo-lock` keeps two Kargo processes from building the same project at the same time. See [Concurrent commands](../../reference/commands/#concurrent-commands).
