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
# entry-point = "com.example.app.main"   # defaults to the top-level `fun main`
# linker-options = ["-lcurl"]
```

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

## Using C libraries

Pass flags to the linker with `linker-options`:

```toml
[target.native]
platforms = ["host"]
linker-options = ["-L/usr/local/lib", "-lsqlite3"]
```

## Moving the toolchain

Kotlin/Native's dependency cache normally lives in `~/.konan`. Kargo keeps it under `~/.kargo/toolchains/konan` instead so everything Kargo downloads is in one place. Set `KONAN_DATA_DIR` to use a different directory.

```sh
kargo toolchain list                    # see installed Kotlin/Native versions
kargo toolchain install native 2.4.21   # pre-install, for example in a Docker image
kargo toolchain remove native 2.4.10    # free disk space
```
