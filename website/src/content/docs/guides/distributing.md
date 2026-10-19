---
title: Distributing applications
description: Ship a JVM application as a runtime image that needs no Java, a GraalVM native executable, or an installer.
---

`kargo package` builds a fat jar, which runs anywhere with `java -jar`, but only where the right Java is installed. To ship a program to people who don't have Java, `--format` builds something else:

```console
$ kargo package --release                         # target/release/jvm/hello-0.1.0-all.jar
$ kargo package --release --format jlink          # the jars, a trimmed Java runtime and launchers
$ kargo package --release --format native-image   # a GraalVM native executable
$ kargo package --release --format installer      # .msi on Windows, .dmg on macOS, .deb on Linux
```

Each prints the path of what it built on stdout, so a script can pick it up. They all need a main class (the project's, or `--bin <name>` for a [`[[bin]]`](../multiple-programs/)), and they all go to `target/<profile>/jvm/package/`.

| Format | Size | Starts in | Needs on the build machine |
|---|---|---|---|
| `jar` | small | JVM startup | nothing |
| `jlink` | ~35 MB and up | JVM startup | a JDK (the project's) |
| `native-image` | ~5 MB and up | milliseconds | GraalVM and a C toolchain |
| `installer` | ~35 MB and up | JVM startup | a JDK 16 or newer, plus the OS's packaging tools |

:::caution[Build on each OS]
A runtime image, a native executable and an installer contain native code for the OS and CPU they were built on: build the Windows one on Windows, the macOS one on macOS (an Apple silicon Mac builds for Apple silicon) and the Linux one on Linux. A CI matrix does it in one go; see [Native executables and installers](../ci/#native-executables-and-installers).
:::

## Runtime images

```console
$ kargo package --release --format jlink
   Compiling hello v0.1.0 (jvm)
   Packaging hello v0.1.0 runtime image (/home/me/hello/target/release/jvm/package/hello-0.1.0)
    Finished `release` profile [optimized] target(s) in 14.08s
/home/me/hello/target/release/jvm/package/hello-0.1.0
$ target/release/jvm/package/hello-0.1.0/bin/hello
Hello, Kotlin!
```

The image is a directory you can zip and ship:

```
hello-0.1.0/
├── bin/
│   ├── hello          # launcher for Linux and macOS
│   └── hello.bat      # launcher for Windows
├── lib/               # the program's jar and its dependencies
└── runtime/           # a Java runtime with only the modules the program uses
```

Kargo asks `jdeps` which JDK modules the program and its dependencies use, and builds the runtime with `jlink --strip-debug --no-header-files --no-man-pages --compress`, from the project's JDK (`[toolchain] jdk`). A Kotlin program that only uses `java.base` gets a runtime of about 30 MB. The launchers pass `[target.jvm] jvm-args` and the profile's `jvm-args` to the program, and arguments after them to `main`.

`jdeps` can't see modules that are only loaded through reflection or a service loader, such as `java.naming` for JNDI or `jdk.crypto.ec` for some TLS connections. List them in [`[package.jlink]`](../../reference/manifest/#packagejlink):

```toml title="kargo.toml"
[package.jlink]
modules = ["java.naming", "jdk.crypto.ec"]
```

If `jdeps` fails on a dependency, set `jdeps = false` and list every module the runtime needs in `modules`.

## Native executables

`--format native-image` compiles the program ahead of time with [GraalVM](https://www.graalvm.org)'s `native-image` into one executable that starts in milliseconds and needs no Java. Pin the GraalVM so Kargo downloads it:

```toml title="kargo.toml"
[toolchain]
graalvm = "25"
```

```console
$ kargo package --release --format native-image
   Compiling hello v0.1.0 (jvm)
 Downloading graalvm-community-25.0.4.1.1.zip
  Installing GraalVM CE 25.0.4.1.1
   Packaging hello v0.1.0 native executable (/home/me/hello/target/release/jvm/package/hello)
 Downloading graalvm-reachability-metadata-1.0.20.zip
...
/home/me/hello/target/release/jvm/package/hello
$ target/release/jvm/package/hello
Hello, Kotlin!
```

Without `[toolchain] graalvm`, Kargo uses the GraalVM in `GRAALVM_HOME`, or a `JAVA_HOME` or `PATH` that has `native-image`. `native-image` prints its progress on stderr, so the executable's path stays alone on stdout. See [GraalVM](../toolchains/#graalvm) for the toolchain itself.

`native-image` needs a C toolchain: `gcc` (or `clang`) and the zlib headers on Linux (`build-essential` and `zlib1g-dev` on Debian and Ubuntu), the Xcode command line tools on macOS, and Visual Studio 2022 or its Build Tools with the C++ workload on Windows. On Windows GraalVM finds Visual Studio itself; when only Visual Studio's `cl.exe` is on `PATH`, without a developer prompt's `INCLUDE`, Kargo hides it so GraalVM can.

### Reflection and resources

A native image only contains the code `native-image` can see is reachable. Code that is only reached through reflection, dynamic proxies, resources or JNI needs configuration, which comes from three places:

1. **Libraries.** Many ship it in their jars under `META-INF/native-image/`, which `native-image` reads.
2. **The [GraalVM reachability metadata repository](https://github.com/oracle/graalvm-reachability-metadata).** It has configuration for libraries that don't ship their own (Netty, Logback, H2 and hundreds more). Kargo downloads it once and adds the entry for each dependency version on the runtime classpath. Turn it off with `metadata-repository = false` under [`[package.native-image]`](../../reference/manifest/#packagenative-image).
3. **Your program.** Run it with GraalVM's tracing agent, which records what it uses:

```console
$ kargo run --native-agent
note: the GraalVM tracing agent writes the program's native-image configuration to native-image
     Running `java com.example.hello.MainKt`
Hello, Kotlin!
```

The agent writes `native-image/reachability-metadata.json` in the project. Each later run adds to it, so run the program through every code path that matters (or run it with different arguments several times), then commit `native-image/`. `kargo package --format native-image` includes it.

### Options

```toml title="kargo.toml"
[package.native-image]
args = ["--enable-url-protocols=https", "-march=native"]
initialize-at-build-time = ["kotlinx.serialization"]
static = true             # Linux only: a fully static executable, linked against musl
```

`args` go to `native-image` as they are. `static = true` needs musl's toolchain (`x86_64-linux-musl-gcc` on `PATH`), and gives an executable that runs on any Linux, even one without glibc such as an Alpine container.

## Installers

`--format installer` runs the JDK's `jpackage` on the program's jars and a runtime built like the [runtime image](#runtime-images)'s:

```toml title="kargo.toml"
[package]
name = "hello"
version = "1.2.0"
description = "Says hello"

[package.installer]
name = "Hello"
vendor = "Example Inc."
icon = { windows = "packaging/hello.ico", macos = "packaging/hello.icns", linux = "packaging/hello.png" }
license-file = "LICENSE"
menu = true
shortcut = true
upgrade-uuid = "0b5b2d6e-8f5c-4a37-9d1e-3a6c2f4b7e90"
bundle-id = "com.example.hello"
```

```console
$ kargo package --release --format installer
   Packaging Hello v1.2.0 msi installer (C:\hello\target\release\jvm\package)
C:\hello\target\release\jvm\package\Hello-1.2.0.msi
```

The installer type defaults to the host's: `.msi` on Windows, `.dmg` on macOS, `.deb` on Linux. Set `type` to `exe`, `pkg` or `rpm` for the others, or to `app-image` for the installed application's directory without an installer, which builds on every OS. See [`[package.installer]`](../../reference/manifest/#packageinstaller) for every key.

What each OS needs:

| OS | Types | Needs |
|---|---|---|
| Windows | `msi`, `exe` | The [WiX Toolset](https://wixtoolset.org) on `PATH` |
| macOS | `dmg`, `pkg` | Xcode command line tools; a signing identity in the keychain for `signing-identity` |
| Linux | `deb`, `rpm` | `dpkg-deb` and `fakeroot` for `.deb`, `rpm-build` for `.rpm` |

Installers need numeric versions, so Kargo passes the numbers at the start of `[package] version` (`1.2.0-SNAPSHOT` becomes `1.2.0`), or `app-version` when you set it. Keep `upgrade-uuid` the same across releases, so that installing a newer `.msi` replaces the older one instead of installing next to it.

:::tip[Command-line tools on Windows]
The Windows launcher of an installed application opens no console window, which suits desktop apps such as [Compose Desktop](../compose/) ones. For a command-line tool, set `console = true` so its output shows in the terminal.
:::

## In a workspace

Run `kargo package --format <format> -p <member>` at the root, or `kargo package --format <format>` in the member's directory. The output goes to `target/<profile>/<member>/jvm/package/`.
