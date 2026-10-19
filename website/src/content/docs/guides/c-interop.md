---
title: C interop
description: Use C libraries from Kotlin/Native on every platform, compile bundled C sources, and call Kotlin from C, Objective-C and Swift.
---

Kotlin/Native calls C directly. Kargo runs the Kotlin/Native `cinterop` tool for each C library you declare in `kargo.toml`, compiles the C files that ship with your project with the toolchain's own clang, and builds C libraries and Apple frameworks from your Kotlin code. No Gradle, CMake or Makefile is involved.

This page goes through it in that order. The basics of the native target are in [Kotlin/Native](../native/).

## A C library on three platforms

One `kargo.toml` builds a sqlite tool for Linux, macOS and Windows. Each platform finds sqlite differently, so each gets its own options:

```toml title="kargo.toml"
[target.native]
platforms = ["linux_x64", "macos_arm64", "mingw_x64"]

[target.native.cinterop.sqlite]
headers = ["sqlite3.h"]          # no .def file needed
package = "sqlite3"
pkg-config = "sqlite3"           # flags from sqlite3.pc, on the machine you build on

[target.native.cinterop.sqlite.platform.mingw_x64]
compiler-options = ["-Ivendor/sqlite/include"]
library-paths = ["vendor/sqlite/lib"]
linker-options = ["-lsqlite3"]

[target.native.cinterop.sqlite.platform.macos_arm64]
linker-options = ["-lsqlite3"]   # macOS ships sqlite
```

```kotlin title="src/Main.kt"
import kotlinx.cinterop.ExperimentalForeignApi
import kotlinx.cinterop.toKString
import sqlite3.sqlite3_libversion

@OptIn(ExperimentalForeignApi::class)
fun main() {
    println("SQLite " + sqlite3_libversion()?.toKString())
}
```

```console
$ kargo run
  Generating sqlite C bindings (linux_x64)
   Compiling app v0.1.0 (native, linux_x64)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.40s
     Running `target/debug/native/linux_x64/app.kexe`
SQLite 3.45.1
```

The bindings use `kotlinx.cinterop` APIs, which need `@OptIn(ExperimentalForeignApi::class)`, or `opt-in = ["kotlinx.cinterop.ExperimentalForeignApi"]` in `[kotlin]`.

`kargo add --cinterop sqlite --header sqlite3.h --pkg-config sqlite3` writes the `[target.native.cinterop.sqlite]` entry and an `interop/sqlite.def` to start from.

## Declaring a C library

Each `[target.native.cinterop.<name>]` table binds one library. Every key is optional:

| Key | Meaning |
|---|---|
| `def` | The [definition file](https://kotlinlang.org/docs/native-definition-file.html), relative to the project root. Default: `interop/<name>.def` |
| `package` | Kotlin package of the bindings. Default: the `package` in the `.def` file |
| `headers` | Headers to bind, as written in `#include`. With `headers` (or `modules`) and no `interop/<name>.def`, no `.def` file is needed |
| `compiler-options` | Options for clang while it reads the headers: `-I` include directories, `-D` defines |
| `linker-options` | Linker options stored in the bindings and applied to every program that uses them: `-L`, `-l` |
| `library-paths` | Directories searched for `static-libraries`, also passed to the linker as `-L`. Relative to the project root |
| `static-libraries` | Static libraries (`libfoo.a`) found in `library-paths` and embedded in the bindings, so programs need no installed copy |
| `frameworks` | Apple frameworks to link (`-framework <name>`). Ignored on other platforms |
| `pkg-config` | A package name, or a list of them, whose compiler and linker flags the bindings use. See [pkg-config](#pkg-config) |
| `link` | `"dynamic"` (default) or `"static"`. See [Static linking](#static-linking) |
| `header-filter`, `excluded-functions`, `strict-enums`, `non-strict-enums`, `modules`, `language` | `.def` properties written in `kargo.toml`. See [`.def` properties](#def-properties) |
| `platform.<platform>` | Options for one platform. See [Per-platform options](#per-platform-options) |

Relative paths in `def` and `compiler-options` resolve against the project root, so `-Iinclude` means `include/` next to `kargo.toml`.

A `.def` file can hold C code after a `---` line, which is compiled into the bindings. That suits a few small helpers; for real C files use [bundled C sources](#bundled-c-sources).

```text title="interop/mathx.def"
headers = twice.h
---
static int add(int a, int b) { return a + b; }
```

Kotlin/Native libraries from Maven that already contain C bindings (Ktor, okio, SQLDelight's driver) need no `cinterop` entry: add them to `[dependencies]`.

## Per-platform options

A `platform.<platform>` table adds options for one Kotlin/Native platform to the common ones. Use the names from `platforms` (`linux_x64`, `macos_arm64`, `mingw_x64`, ...; `linuxX64` works too). `host` is not allowed here: name the platform.

Under a cinterop entry the table takes `compiler-options`, `linker-options`, `static-libraries`, `library-paths` and `frameworks`:

```toml title="kargo.toml"
[target.native.cinterop.curl]
headers = ["curl/curl.h"]
package = "libcurl"

[target.native.cinterop.curl.platform.linux_x64]
compiler-options = ["-I/usr/include", "-I/usr/include/x86_64-linux-gnu"]
linker-options = ["-L/usr/lib/x86_64-linux-gnu", "-lcurl"]

[target.native.cinterop.curl.platform.macos_arm64]
compiler-options = ["-I/opt/homebrew/opt/curl/include"]
linker-options = ["-L/opt/homebrew/opt/curl/lib", "-lcurl"]

[target.native.cinterop.curl.platform.mingw_x64]
compiler-options = ["-IC:/msys64/ucrt64/include"]
linker-options = ["-LC:/msys64/ucrt64/lib", "-lcurl"]
```

`[target.native]` has the same tables for the linker options of your program, for libraries that are not bound with `cinterop`. They take `linker-options`, `static-libraries`, `library-paths` and `frameworks`:

```toml title="kargo.toml"
[target.native]
platforms = ["linux_x64", "macos_arm64", "mingw_x64"]
linker-options = ["-lm"]                  # every platform

[target.native.platform.macos_arm64]
frameworks = ["CoreFoundation"]

[target.native.platform.mingw_x64]
linker-options = ["-lws2_32"]
```

:::note
Kotlin/Native compiles against its own sysroot, not the system's. On Linux, headers and libraries in `/usr/include` and `/usr/lib` need `-I` and `-L` options (or `pkg-config`, which gives them).
:::

## `.def` properties

These keys write `.def` file properties from `kargo.toml`, so a library rarely needs its own `.def` file:

| Key | `.def` property | Meaning |
|---|---|---|
| `header-filter` | `headerFilter` | Globs of the headers whose declarations are bound; others are only read |
| `excluded-functions` | `excludedFunctions` | Functions left out of the bindings |
| `strict-enums` | `strictEnums` | C enums bound as Kotlin `enum class`es |
| `non-strict-enums` | `nonStrictEnums` | C enums bound as integer constants |
| `modules` | `modules` | Clang modules to bind, such as Objective-C frameworks |
| `language` | `language` | `"C"` or `"Objective-C"` |

```toml title="kargo.toml"
[target.native.cinterop.sdl]
headers = ["SDL2/SDL.h"]
package = "sdl"
header-filter = ["SDL2/*"]
excluded-functions = ["SDL_main"]
```

Kargo writes them with the contents of the project's `.def` file, if there is one, into `target/<profile>/native/<platform>/cinterop/<name>.def`. A key set in both places takes the `.def` file's value.

## pkg-config

`pkg-config = "<package>"` takes the compiler and linker flags of an installed library from its `<package>.pc` file:

```toml title="kargo.toml"
[target.native.cinterop.curl]
headers = ["curl/curl.h"]
package = "libcurl"
pkg-config = "libcurl"
```

Kargo reads the `.pc` files itself, including the packages they require, so no `pkg-config` program is needed. It looks in this order:

1. The directories in `PKG_CONFIG_PATH`.
2. The directories an installed `pkg-config` (or `pkgconf`) searches.
3. The usual places: Homebrew (`/opt/homebrew`, `/usr/local`) and MacPorts on macOS; `/usr`, `/usr/local` and Linuxbrew on Linux; MSYS2 (`C:/msys64/ucrt64`, `mingw64`, `clang64` and `MSYSTEM_PREFIX`) on Windows; and vcpkg's `installed/<triplet>` under `VCPKG_ROOT`.

Unlike `pkg-config`, Kargo keeps `-I/usr/include` and `-L/usr/lib/...`, which the Kotlin/Native sysroot needs. pkg-config describes the machine you build on, so its flags only apply to the host platform. Give the other platforms their options in `platform.<platform>` tables.

A missing package stops the build before anything is compiled:

```console
$ kargo build
error: C library `sqlite3` of [target.native.cinterop.sqlite] not found: no pkg-config file `sqlite3.pc` was found (searched /usr/lib/x86_64-linux-gnu/pkgconfig, /usr/share/pkgconfig); install its development package (such as `apt install libsqlite3-dev` or `dnf install sqlite3-devel`), set PKG_CONFIG_PATH to the directory holding `sqlite3.pc`, or give its options in [target.native.cinterop.sqlite.platform.linux_x64] instead of `pkg-config`
```

## Static linking

With `link = "static"`, every `-l<name>` option whose `lib<name>.a` is in a library directory (`library-paths`, `-L` options and pkg-config's `-L` flags) is embedded in the bindings instead. The executable then runs on machines without the library. Libraries without a static archive, such as the system's `-lm` or `-lpthread`, stay dynamic. pkg-config adds the package's `Libs.private`, which a static link needs.

```toml title="kargo.toml"
[target.native.cinterop.sqlite]
headers = ["sqlite3.h"]
package = "sqlite3"
pkg-config = "sqlite3"
link = "static"
```

To embed a library you vendor, list it in `static-libraries`:

```toml title="kargo.toml"
[target.native.cinterop.sqlite.platform.mingw_x64]
compiler-options = ["-Ivendor/sqlite/include"]
library-paths = ["vendor/sqlite/lib/mingw_x64"]
static-libraries = ["libsqlite3.a"]
```

`link = "static"` fails when it finds no static library at all, so a missing archive doesn't silently produce a dynamically linked program.

## Bundled C sources

`[target.native.c-sources]` compiles the C, C++ and Objective-C files of your project with the clang that comes with Kotlin/Native, for each platform with that platform's sysroot, into a static library that every native binary links. It works like Rust's `cc` crate:

```toml title="kargo.toml"
[target.native]
platforms = ["host"]

[target.native.c-sources]            # compiles c/**/*.c with -Ic
defines = ["FACTOR=2"]

[target.native.c-sources.platform.mingw_x64]
defines = ["BONUS=100"]

[target.native.cinterop.mathx]       # the bindings for c/mathx.h
headers = ["mathx.h"]
package = "mathx"
```

```c title="c/mathx.h"
int mathx_scale(int x);
```

```c title="c/mathx.c"
#include "mathx.h"
#ifndef BONUS
#define BONUS 0
#endif
int mathx_scale(int x) { return x * FACTOR + BONUS; }
```

```kotlin title="src/Main.kt"
import kotlinx.cinterop.ExperimentalForeignApi

@OptIn(ExperimentalForeignApi::class)
fun main() = println("scaled " + mathx.mathx_scale(21))
```

```console
$ kargo run
  Generating mathx C bindings (mingw_x64)
   Compiling app v0.1.0 (native, mingw_x64, C sources)
   Compiling app v0.1.0 (native, mingw_x64)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.00s
     Running `target\debug\native\mingw_x64\app.exe`
scaled 142
```

| Key | Meaning |
|---|---|
| `sources` | Files, directories and globs (gitignore syntax, such as `vendor/**/*.c`), relative to the project root. A directory contributes its `.c`, `.cc`, `.cpp`, `.cxx`, `.m` and `.mm` files. Default: `["c"]` |
| `include-dirs` | Include directories (`-I`), relative to the project root. Every cinterop entry gets them too, so its `headers` are found. Default: `["c"]` |
| `defines` | Macros: `NAME` or `NAME=value` |
| `compiler-options` | More clang options, such as `-std=c11` or `-Wall` |
| `platform.<platform>` | `defines` and `compiler-options` for one platform |

C++ files (`.cc`, `.cpp`, `.cxx`, `.mm`) are compiled with `clang++`. Debug profiles compile with `-O0 -g`, optimized ones with `-O2`.

The library is `target/<profile>/native/<platform>/c/lib<package>_c.a`. Executables, test runners, shared and static libraries and frameworks link it; with `kind = "library"` it is packed into the klib, so whatever links the klib gets the C code. It is rebuilt when a source, a file in an include directory, an option or the Kotlin version changes.

## Calling Kotlin from C

`kind = "shared"` or `kind = "static"` builds a C library from your Kotlin code. Kotlin/Native also writes a C header for it, which Kargo copies into `target/<profile>/native/<platform>/include/`:

```toml title="kargo.toml"
[package]
name = "mathlib"
version = "0.1.0"

[target.native]
platforms = ["linux_x64"]
kind = "shared"
```

```kotlin title="src/Math.kt"
@file:OptIn(kotlin.experimental.ExperimentalNativeApi::class)

import kotlin.native.CName

@CName("mathlib_add")
fun add(a: Int, b: Int): Int = a + b
```

```console
$ kargo build
   Compiling mathlib v0.1.0 (native, linux_x64)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.12s
$ ls target/debug/native/linux_x64/include/
libmathlib_api.h
```

```c title="main.c"
#include "libmathlib_api.h"
#include <stdio.h>

int main(void) {
    printf("%d\n", mathlib_add(20, 22));
    return 0;
}
```

```sh
cc main.c -Itarget/debug/native/linux_x64/include -Ltarget/debug/native/linux_x64 -lmathlib -o main
```

Functions marked `@CName` get that plain C name. Everything else public is reachable through the structure that the header's `..._symbols()` function returns. The files are named `lib<name>.so`, `lib<name>.dylib`, `<name>.dll` (with `<name>_api.h`) or `lib<name>.a`.

## Calling Kotlin from Swift and Objective-C

`kind = "framework"` builds an Apple framework for each platform in `platforms`, which must all be Apple platforms. Frameworks are built on macOS.

```toml title="kargo.toml"
[package]
name = "shared"
group = "com.example"
version = "0.1.0"

[target.native]
platforms = ["ios_arm64", "ios_simulator_arm64", "ios_x64", "macos_arm64"]
kind = "framework"
bundle-id = "com.example.Shared"     # default: <group>.<name>
export = ["org.jetbrains.kotlinx:kotlinx-datetime"]

[dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
```

```kotlin title="src/Greeting.kt"
fun greeting(name: String): String = "Hello, $name!"
```

The framework is `target/<profile>/native/<platform>/shared.framework` (dashes in the package name become underscores, since the name is a Swift module name). Swift sees top-level functions on a class named after their file:

```swift title="ContentView.swift"
import shared

let text = GreetingKt.greeting(name: "Swift")
```

| Key | Meaning |
|---|---|
| `bundle-id` | The framework's `CFBundleIdentifier`. Default: `<group>.<name>` when `[package] group` is set |
| `export` | Dependencies (`group:artifact`) whose API the framework's header includes, so Swift can use their types. Also works with `kind = "shared"` and `"static"` |

To use one framework for devices and simulators, combine them into an XCFramework:

```console
$ kargo build --release --xcframework
   Compiling shared v0.1.0 (native, ios_arm64)
   Compiling shared v0.1.0 (native, ios_simulator_arm64)
   Compiling shared v0.1.0 (native, ios_x64)
   Compiling shared v0.1.0 (native, macos_arm64)
   Packaging shared.xcframework
    Finished `release` profile [optimized] target(s) in 3m 02s
```

Kargo merges the frameworks of the same slice (the arm64 and x64 iOS simulators) with `lipo`, then runs `xcodebuild -create-xcframework`. The result is `target/<profile>/native/shared.xcframework`; drag it into an Xcode project's Frameworks, Libraries, and Embedded Content. It needs Xcode's command-line tools.

## When something is missing

Errors from clang and the linker are shown as they are, with their file and line. Kargo adds a note on what to change for the two most common failures:

```console
$ kargo build
  Generating mathx C bindings (linux_x64)
Exception in thread "main" java.lang.Error: /tmp/4645017247209148547.c:1:10: fatal error: 'mathx.h' file not found
note: the C header `mathx.h` was not found: add the directory that holds it to `compiler-options` (`-I<dir>`) of [target.native.cinterop.mathx] or of its `platform.<platform>` table, name the library's package in `pkg-config`, or install the library's development files
error: could not generate the `mathx` C bindings for linux_x64 due to previous errors
```

```console
$ kargo build
ld.lld: error: undefined symbol: mathx_scale
note: the linker did not find `mathx_scale`: link the C library that defines it with `pkg-config`, `linker-options = ["-l<library>"]` or `static-libraries` in its [target.native.cinterop.<name>] entry (or [target.native.platform.<platform>]), or add its sources to [target.native.c-sources]
error: could not compile `app` for linux_x64 due to previous errors
```

## Up-to-date checks

Bindings are generated once per platform into `target/<profile>/native/<platform>/cinterop/<name>.klib`. They are regenerated when the `.def` file, the options (pkg-config's included), a static library, a file in a project-relative `-I` directory or the Kotlin version changes. Run `kargo clean` after updating a system library.

## Publishing

`kargo publish` uploads each cinterop klib next to the platform's klib with the classifier `cinterop-<name>`, the way Kotlin Multiplatform libraries do. With `[target.native.c-sources]` the C library travels inside the klib. See [Publishing](../publishing/).
