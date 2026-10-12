---
title: Kotlin/Native
description: Build native executables and libraries for Linux, macOS, Windows, iOS and more.
---

Kotlin/Native compiles Kotlin to machine code. The result is a self-contained executable or library that doesn't need a JVM.

## Setup

```toml title="kargo.toml"
[target.native]
platforms = ["host"]       # the machine you're building on
kind = "executable"        # or "library", "shared", "static"
# entry-point = "com.example.app.main"   # defaults to the top-level `fun main`; see also [[bin]]
# linker-options = ["-lcurl"]

# [target.native.cinterop.sqlite]       # bind a C library, see below
```

In an existing project, `kargo target add native` writes this section for you: `kind = "executable"` when the sources have a top-level `fun main`, `"library"` otherwise (override with `--app` or `--lib`). See [`kargo target`](../../reference/commands/#kargo-target).

You don't need to install anything. On the first native build Kargo downloads the Kotlin/Native distribution that matches `[kotlin] version` into `~/.kargo/toolchains/kotlin-native/<version>`, plus its LLVM toolchain and sysroot into `~/.kargo/toolchains/konan`. The first build therefore takes a few minutes; later builds are quick.

Output goes to `target/<profile>/native/<platform>/`.

```console
$ kargo new tool -t native
$ cd tool
$ kargo run
   Compiling tool v0.1.0 (native)
Hello, Kotlin!
$ kargo build --release
$ ./target/release/native/linux_x64/tool.kexe   # tool.exe on Windows
```

## Platforms

`platforms` accepts these names (camelCase like `linuxX64` and dashes like `linux-x64` also work):

| Family | Platforms |
|---|---|
| Your machine | `host` |
| Linux | `linux_x64`, `linux_arm64` |
| Windows | `mingw_x64` |
| macOS | `macos_x64`, `macos_arm64` |
| iOS | `ios_arm64`, `ios_x64`, `ios_simulator_arm64` |
| watchOS / tvOS | `watchos_arm64`, `watchos_simulator_arm64`, `tvos_arm64`, `tvos_simulator_arm64` |
| Android NDK | `android_arm64`, `android_arm32`, `android_x64` |

Apple targets can only be built on macOS. `kargo run` runs the build for the host platform, so include `host` in `platforms` if you want to run the program locally.

## Output kinds

| `kind` | Produces |
|---|---|
| `executable` | A program with a `main` function |
| `library` | A `.klib` that other Kotlin/Native code can depend on |
| `shared` | A shared C library (`.so`, `.dylib`, `.dll`) plus a C header |
| `static` | A static C library (`.a`, `.lib`) plus a C header |

## Several executables

Kotlin/Native needs one entry function per executable. With several top-level `fun main`s (each in its own package), run any of them with `kargo run --entry-point com.example.tools.main`, or declare a `[[bin]]` per program:

```toml title="kargo.toml"
[[bin]]
name = "migrate"
path = "src/tools/Migrate.kt"   # or entry-point = "com.example.tools.main"
```

`kargo build` then also links `target/<profile>/native/<platform>/bin/migrate.kexe` (`migrate.exe` on Windows), and `kargo run --bin migrate` runs it. See [Several programs in one project](../multiple-programs/).

## Using C libraries

`platform.posix` and the other platform libraries that ship with Kotlin/Native are always available. To call any other C library (sqlite, curl, SDL, your own code), declare it under `[target.native.cinterop.<name>]`. Kargo runs Kotlin/Native's `cinterop` tool on it and puts the generated bindings on the compile path of your program and its tests.

```toml title="kargo.toml"
[target.native]
platforms = ["host"]

[target.native.cinterop.sqlite]
package = "sqlite3"                    # Kotlin package of the bindings
compiler-options = ["-I/usr/include"]  # passed to clang while reading the headers
linker-options = ["-lsqlite3"]         # used when linking any program that uses the bindings
```

```text title="interop/sqlite.def"
headers = sqlite3.h
```

```kotlin title="src/Main.kt"
import kotlinx.cinterop.ExperimentalForeignApi
import sqlite3.sqlite3_libversion
import kotlinx.cinterop.toKString

@OptIn(ExperimentalForeignApi::class)
fun main() {
    println("SQLite " + sqlite3_libversion()?.toKString())
}
```

Each entry takes these keys, all optional:

| Key | Meaning |
|---|---|
| `def` | The [definition file](https://kotlinlang.org/docs/native-definition-file.html), relative to the project root. Default: `interop/<name>.def` |
| `package` | Kotlin package of the bindings. Default: the `package` in the `.def` file |
| `headers` | Headers to bind, as you would write them in `#include`. With `headers` and no `interop/<name>.def`, no `.def` file is needed |
| `compiler-options` | Options for clang when it reads the headers, such as `-I` include directories and `-D` defines |
| `linker-options` | Linker options stored in the bindings and applied to every program that uses them, such as `-L` and `-l` |

A `.def` file can also contain C code after a `---` line. It is compiled into the bindings, which is handy for small helpers:

```toml title="kargo.toml"
[target.native.cinterop.mathx]
package = "mathx"
compiler-options = ["-Iinclude"]
```

```text title="interop/mathx.def"
headers = twice.h
---
static int add(int a, int b) { return a + b; }
```

```c title="include/twice.h"
static inline int twice(int x) { return 2 * x; }
```

```kotlin title="src/Main.kt"
import kotlinx.cinterop.ExperimentalForeignApi

@OptIn(ExperimentalForeignApi::class)
fun main() = println("answer " + mathx.twice(mathx.add(20, 1)))
```

```console
$ kargo run
  Generating mathx C bindings (linux_x64)
   Compiling app v0.1.0 (native, linux_x64)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 14.68s
     Running `target/debug/native/linux_x64/app.kexe`
answer 42
```

Relative paths in `compiler-options` and `def` resolve against the project root, so `-Iinclude` points at `include/` next to `kargo.toml`. The C bindings are generated once per platform into `target/<profile>/native/<platform>/cinterop/<name>.klib` and regenerated when the `.def` file, the options, the Kotlin version or a file in a project-relative `-I` directory changes. Run `kargo clean` after changing a system header.

The bindings use `kotlinx.cinterop` APIs, which need `@OptIn(ExperimentalForeignApi::class)`, or `opt-in = ["kotlinx.cinterop.ExperimentalForeignApi"]` in `[kotlin]`.

Kotlin/Native libraries from Maven that already contain C bindings (for example okio) need no `cinterop` entry: add them to `[dependencies]`.

### Linker options

`[target.native] linker-options` passes flags to the linker of your program, for libraries that are not bound with `cinterop`:

```toml
[target.native]
platforms = ["host"]
linker-options = ["-L/usr/local/lib", "-lsqlite3"]
```

## Build speed

Most of a small Kotlin/Native build is the standard library: unless told otherwise, the compiler loads and lowers all of it again on every build. For debug builds Kargo tells it to link a precompiled copy instead, as the Gradle plugin does. Rebuilding a small program takes about half the time (10 seconds instead of 5 on one machine, 16 instead of 6 on a slower one).

```console
$ kargo build
   Compiling tool v0.1.0 (native, linux_x64)
note: building the Kotlin/Native standard library cache for linux_x64 (one time per Kotlin version, takes a few minutes); later builds are much faster
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 97.17s
```

- **One-time cost.** The first build that needs the cache builds it, which adds one to two minutes. It's kept per Kotlin/Native version and platform in `~/.kargo/toolchains/kotlin-native/<version>/klib/cache`, a few tens of megabytes each, and `kargo toolchain remove native <version>` removes it with the distribution.
- **Debug builds only.** `--release` (any profile with `optimize = true`) optimizes the whole program, standard library included, so it can't use precompiled code and builds as slowly as before. `kargo check` and `kind = "library"` build a klib, which doesn't link the standard library.
- **Debug info.** Builds with `debug = true` (the default in `dev`) and builds without it each get their own cache.
- **Turning it off.** Set `KARGO_NATIVE_CACHE=false` (or `0`, `no`, `off`) to compile without it, for example when the toolchain directory is read-only. Switching it doesn't trigger a rebuild, because the output is the same.
- **CI.** The cache is under `~/.kargo`, so a cached `~/.kargo` [keeps it between runs](../ci/#kotlinnative-cache). A job that starts from an empty home pays for building it on every run; set `KARGO_NATIVE_CACHE=false` there.

## Moving the toolchain

Kotlin/Native's dependency cache normally lives in `~/.konan`. Kargo keeps it under `~/.kargo/toolchains/konan` instead so everything Kargo downloads is in one place. Set `KONAN_DATA_DIR` to use a different directory.

```sh
kargo toolchain list                    # see installed Kotlin/Native versions
kargo toolchain install native 2.4.21   # pre-install, for example in a Docker image
kargo toolchain remove native 2.4.10    # free disk space
```
