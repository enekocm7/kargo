---
title: Managing JDKs
description: Pin a JDK per project, see which toolchains Kargo installs on its own, and manage installed JDKs, GraalVMs, Kotlin/Native and Gradle distributions.
---

Kargo can use the JDK you already have, or download and manage JDKs for you, per project. A command that needs a toolchain it doesn't find installs it, so a new machine needs nothing but Kargo.

## Without a JDK

With the default `jdk = "system"`, Kargo uses the JDK on `JAVA_HOME`, then `java` on `PATH`. If there is neither, it downloads a Temurin JDK into `~/.kargo/toolchains/jdk/` and uses that, so this works on a machine without Java:

```console
$ kargo new app && cd app
$ kargo run
note: no JDK on JAVA_HOME or PATH; installing Temurin JDK 21 for Kargo (KARGO_AUTO_INSTALL=0 turns this off)
 Downloading jdk-21.0.12.1_1.zip
  Installing JDK jdk-21.0.12.1+1 (Temurin)
   ...
   Compiling app v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 26.88s
     Running `java com.example.app.MainKt`
Hello, Kotlin!
```

Kargo picks Temurin 21, the LTS it builds with by default. A project that needs a newer one gets the next LTS: `jvm-target = "25"` gets JDK 25. Kotlin versions before 2.1.10 can't run on JDK 25, so they always get 21. Once a managed JDK is installed, later commands use it without a download, offline too. If you have several, they use the newest one that fits the project.

A JDK on `JAVA_HOME` or `PATH` always wins, so nothing changes if you have one. A `JAVA_HOME` that points at a broken JDK is still an error, not a reason to download another one.

With `--offline` or `--locked`, Kargo never chooses and downloads a JDK by itself. The error names the command that installs it, `kargo toolchain install jdk 21`. Under `--locked` it also suggests pinning that JDK with `[toolchain] jdk`, which makes the build reproducible.

## What installs automatically

| Toolchain | Needed by | Installs automatically |
|---|---|---|
| JDK (`[toolchain] jdk = "system"`) | `build`, `run`, `test`, `check`, `bench`, `package`, `fmt`, `lint`, `doc`, `script`, `lsp`, `dap`, `install` | Yes, when no JDK is on `JAVA_HOME` or `PATH`: Temurin 21, or the LTS the project needs. Not with `--offline` or `--locked` |
| JDK (pinned `[toolchain] jdk = "21"`) | The same commands | Yes, on first use. Not with `--offline` |
| GraalVM (`[toolchain] graalvm`) | `package --format native-image`, `run --native-agent` | Yes, when pinned. With `system`, Kargo looks on `GRAALVM_HOME`, `JAVA_HOME` and `PATH` |
| GraalVM reachability metadata | `package --format native-image` | Yes |
| JetBrains Runtime | `run --hot-reload` of a Compose app | Yes, the JBR of the project's Java version, into `~/.kargo/toolchains/jbr/`. Offline, the app runs on the project's JDK instead. Other programs run on the project's JDK, with enhanced redefinition when it is a JBR. See [Hot reload](../compose/#hot-reload) |
| Kotlin/Native | Builds of the `native` target | Yes |
| Gradle | Builds of the `android` target without a `gradlew` | Yes |
| Android SDK (`[toolchain] android-sdk = "36"`) | Builds of the `android` target | Yes, once its licenses are accepted: Kargo asks on a terminal, and elsewhere stops and names `kargo toolchain install android-sdk --accept-licenses`. With `system`, Kargo uses `ANDROID_HOME` and installs nothing. See [Android SDK](../android/#android-sdk) |
| Kotlin LSP, kotlin-language-server | `lsp --server kotlin-lsp`, `lsp --server kls` | Yes |
| kotlin-debug-adapter | `dap` | Yes |
| Kotlin compiler, compiler plugins | Every build | Yes, from Maven Central like a dependency |
| ktlint, detekt, Dokka, JUnit launcher, JMH | `fmt`, `lint`, `doc`, `test`, `bench` | Yes, from Maven Central like a dependency |
| Node.js, a browser | Running Wasm programs and tests | No: install them yourself |

Every install prints a status line, verifies the download's SHA-256 (SHA-1 for the Android SDK, the only checksum Google publishes) and takes the toolchain's [lock](#parallel-installs-and-retries). It never prompts, so CI behaves like your terminal, except for the Android SDK's license terms, which only you can accept. Downloads go through [`[toolchain-mirrors]`](../offline-and-mirrors/#toolchain-mirrors).

### Turning automatic installs off

Set `KARGO_AUTO_INSTALL=0`, or put this in `~/.kargo/config.toml` (or a project's `.kargo/config.toml`):

```toml title="~/.kargo/config.toml"
[toolchain]
auto-install = false
```

The environment variable wins over the file, and `KARGO_AUTO_INSTALL=1` turns installs back on. With installs off, a command that needs a toolchain that isn't installed stops and names the command that installs it:

```console
$ KARGO_AUTO_INSTALL=0 kargo build
error: no Java installation found on JAVA_HOME or PATH, and automatic toolchain installs are off (KARGO_AUTO_INSTALL=0, or [toolchain] auto-install = false in config.toml); run `kargo toolchain install jdk 21`, or install a JDK and set JAVA_HOME
```

`kargo toolchain install` always installs, because you asked for that toolchain by name. Tools that come from Maven, such as the Kotlin compiler and ktlint, are dependencies, not toolchains, so they still download.

## Pinning a JDK

```toml title="kargo.toml"
[toolchain]
jdk = "21"              # the newest Temurin 21
# jdk = "21.0.5+11"     # an exact release
# jdk = "system"        # the default: JAVA_HOME, then `java` on PATH
```

With anything other than `system`, Kargo downloads that [Eclipse Temurin](https://adoptium.net) JDK from Adoptium the first time it's needed, verifies the published SHA-256 and installs it under `~/.kargo/toolchains/jdk/<release>`. That JDK is then used for compiling, running, testing and for Android's Gradle build.

A major version like `"21"` uses the newest release of 21 you have installed, and downloads one if you have none.

Kargo also checks that `[target.jvm] jvm-target` is not newer than the JDK that builds the project (the pinned one, or `JAVA_HOME`/`PATH` with `jdk = "system"`), so you get a clear error with a hint to set `[toolchain] jdk`, instead of a confusing compiler failure. If you don't set `jvm-target`, it defaults to 25, lowered to the JDK's version when the JDK is older, so pinning `jdk = "21"` alone is enough.

### Older Kotlin versions on new JDKs

The Kotlin compiler runs on the same JDK, and compilers older than Kotlin 2.1.10 can't run on JDK 25 or newer. Kargo checks this before building and stops with an error instead of the compiler's stack trace:

```console
$ kargo build
error: Kotlin 2.1.0 cannot run on JDK 25 (at /usr/lib/jvm/jdk-25): its compiler supports up to JDK 24; set `[toolchain] jdk = "21"` in kargo.toml, or use Kotlin 2.1.10 or newer
```

Either upgrade `[kotlin] version`, or pin an older JDK for the project:

```toml title="kargo.toml"
[kotlin]
version = "2.1.0"

[toolchain]
jdk = "21"
```

:::tip[Why pin?]
Everyone on the team, and CI, builds with the same JDK, without anyone having to install it first.
:::

## Requiring a Kargo version

`[toolchain] kargo` says which Kargo versions the project builds with, a [semver requirement](../../reference/manifest/#toolchain) such as `">=0.1.2"`. A Kargo outside it stops with `this project needs kargo >=0.1.2 ... run `kargo self update``, so a CI job or a teammate with an old Kargo gets that message instead of a puzzling error about a new `kargo.toml` key.

```toml title="kargo.toml"
[toolchain]
kargo = ">=0.1.2"
```

## The `kargo toolchain` command

```console
$ kargo toolchain available          # JDK versions you can install (LTS releases are marked)
$ kargo toolchain available android-sdk   # Android SDK platforms and build-tools you can install
$ kargo toolchain install jdk 21     # newest 21
$ kargo toolchain install jdk 21.0.5+11
$ kargo toolchain install graalvm 25 # newest GraalVM CE for JDK 25 (or: graalvm oracle-25)
$ kargo toolchain install native 2.4.21
$ kargo toolchain install gradle     # the Gradle this Android project needs (or: gradle 9.6.0)
$ kargo toolchain install android-sdk --accept-licenses   # the Android SDK of [toolchain] android-sdk (or: android-sdk 36)
$ kargo toolchain install android-sdk 36 --emulator   # also the emulator, avdmanager and a system image
$ kargo toolchain install            # everything the current project needs (a JDK too, when there is none; the Kotlin/Native LLVM and sysroots of its platforms)
$ kargo toolchain list               # installed toolchains, disk usage, and what this project uses
$ kargo toolchain which              # the toolchains this project resolves to
$ kargo toolchain remove jdk 17      # every JDK 17 release
$ kargo toolchain remove jdk 17.0.13+11
$ kargo toolchain remove graalvm 25  # every GraalVM CE 25 release
$ kargo toolchain remove native 2.4.10
$ kargo toolchain remove gradle 9.5.0
$ kargo toolchain remove android-sdk 35              # the platform for API 35
$ kargo toolchain remove android-sdk "build-tools;35.0.0"
$ kargo toolchain install kotlin-lsp # the Kotlin language server `kargo lsp --server kotlin-lsp` runs (or: kls)
```

`kargo toolchain ls` and `kargo toolchain rm` are aliases.

## GraalVM

[`kargo package --format native-image`](../distributing/#native-executables) and `kargo run --native-agent` need a [GraalVM](https://www.graalvm.org). Pin one like a JDK:

```toml title="kargo.toml"
[toolchain]
graalvm = "25"             # the newest GraalVM Community Edition for JDK 25
# graalvm = "oracle-25"    # Oracle GraalVM, under the GraalVM Free Terms and Conditions
# graalvm = "system"       # the default: GRAALVM_HOME, then a JAVA_HOME or PATH with native-image
```

Kargo looks the release up with the [foojay Disco API](https://api.foojay.io), downloads it the first time it's needed, verifies its SHA-256 and installs it under `~/.kargo/toolchains/graalvm/<distribution>-<version>`. GraalVM only builds native images; `[toolchain] jdk` still compiles, runs and tests the project.

Kargo also downloads the [GraalVM reachability metadata repository](https://github.com/oracle/graalvm-reachability-metadata) (version 1.0.20) into `~/.kargo/toolchains/graalvm-reachability-metadata/` the first time a native image needs it.

## Kotlin/Native

Kotlin/Native distributions are tied to `[kotlin] version` and installed automatically on the first native build, under `~/.kargo/toolchains/kotlin-native/<version>`. Their LLVM toolchains and sysroots go in `~/.kargo/toolchains/konan` (override with `KONAN_DATA_DIR`); the first link of a platform downloads them, or `kargo toolchain install` does for every platform of the project. The precompiled standard library that speeds up [debug builds](../native/#build-speed) is built into the distribution itself (`klib/cache`), so `kargo toolchain remove native <version>` removes it too.

## Gradle

Android builds run the Android Gradle Plugin on Gradle. Kargo downloads the Gradle version that plugin needs on the first Android build, verifies its SHA-256 and installs it under `~/.kargo/toolchains/gradle/<version>`. A `gradlew` in the project root takes precedence, and `gradle-version` under `[target.android]` pins another version. See [Android](../android/#gradle).

## Android SDK

Android builds also need an Android SDK. Pin one like a JDK, by platform API level, and Kargo installs that platform, the build-tools of the Android Gradle Plugin in use, and platform-tools:

```toml title="kargo.toml"
[toolchain]
android-sdk = "36"          # Kargo installs platform 36 under ~/.kargo/toolchains/android-sdk
# android-sdk = "system"    # the default: ANDROID_HOME, then ANDROID_SDK_ROOT, then Android Studio's location
```

To run apps on a virtual device, add `--emulator`: `kargo toolchain install android-sdk 36 --emulator` also installs the emulator, `cmdline-tools;latest` and a system image for that API level (see [Devices and emulators](../android/#devices-and-emulators)).

Kargo reads Google's repository manifest on `dl.google.com`, verifies every download against the SHA-1 it lists (the only checksum Google publishes for these packages), and keeps every installed platform in the one SDK directory. The packages are under Google's license terms: Kargo shows them and asks, or takes `kargo toolchain install android-sdk --accept-licenses` in CI, and never accepts them by itself. `compile-sdk` under `[target.android]` defaults to this version. See [Android SDK](../android/#android-sdk).

## Kotlin language servers

[`kargo lsp`](../editors/#language-server) uses [Kargo's own Kotlin server](../kotlin-server/) by default, which is built into `kargo` and needs no toolchain: it only downloads the standard library's sources jar into the Maven cache. With `--server`, it runs one of these Kotlin language servers behind its own instead, downloaded on first use like the other toolchains:

| Server | `kargo lsp --server` | Version | Installed under |
|---|---|---|---|
| JetBrains' [Kotlin LSP](https://github.com/Kotlin/kotlin-lsp) | `kotlin-lsp` | 263.6379.0 | `~/.kargo/toolchains/kotlin-lsp/<version>` |
| fwcd's [kotlin-language-server](https://github.com/fwcd/kotlin-language-server) | `kls` | 1.3.13 | `~/.kargo/toolchains/kotlin-language-server/<version>` |

Kargo pins one version of each and checks the download's SHA-256: the one JetBrains publishes next to the archive for the Kotlin LSP, and one built into Kargo for kotlin-language-server. The Kotlin LSP brings its own Java runtime. kotlin-language-server runs on the project's JDK, or, when that is newer than 21 (its Kotlin compiler doesn't support newer ones), on a Temurin 21 that Kargo [installs](#pinning-a-jdk) like any managed JDK. `kargo toolchain install kotlin-lsp` downloads it ahead of time, for example before going offline.

[`kargo dap`](../editors/#debugging), the debug adapter, runs fwcd's [kotlin-debug-adapter](https://github.com/fwcd/kotlin-debug-adapter) 0.4.4 the same way: Kargo downloads it on first use into `~/.kargo/toolchains/kotlin-debug-adapter/<version>`, checks it against a SHA-256 built into Kargo, and runs it on the project's JDK. `kargo toolchain install kotlin-debug-adapter` downloads it ahead of time.

## The Kotlin compiler

You don't install the Kotlin compiler. Kargo downloads the version in `[kotlin] version` from Maven Central into its cache. To try a new Kotlin release, change the version and rebuild.

## The TypeScript compiler

The `.ts` modules of `js/` on the [Wasm](../wasm/#your-own-javascript-and-typescript) and [JS](../js/) targets are compiled with the `typescript` npm package, which Kargo installs with npm, pnpm or bun into `target/typescript/` and runs on Node.js (or bun). Without configuration it is the newest release, whatever the npm registry's `latest` tag points at, so a new TypeScript major version arrives without a Kargo update. Kargo looks the version up once and pins it in `kargo.lock`, so a project keeps one compiler until you run `kargo update`:

```console
$ kargo build
   Compiling 3 TypeScript modules (tsc 7.0.2)
```

To stay on a compiler on purpose, pin it in `kargo.toml` with an exact version, a range or a dist-tag:

```toml title="kargo.toml"
[toolchain]
typescript = "5.9.3"   # or "^6", "~5.9", "next"; default "latest"
```

An exact version needs no registry lookup, which helps offline and on a mirror that lacks the registry's metadata. Kargo compiles with the same `tsc` command line and `tsconfig.json` for every version. See [`kargo.lock`](../../reference/lockfile/#build-tools) for what is recorded.

## Parallel installs and retries

Toolchains are shared by every project on the machine, so two Kargo processes can need the same one at once. Kargo locks a toolchain while it installs or removes it: the second process prints a `Blocking` status and waits, then uses what the first one installed. Nothing is downloaded or deleted twice.

Downloads are retried when the connection drops or the server answers with HTTP 408, 429, 500, 502, 503 or 504, up to five times with a short backoff. Other failures, such as a 404 or a checksum mismatch, stop at once. On Windows, an install also retries for a moment when a file in it is briefly in use, which antivirus scanners often cause.

## Offline and Docker

Pre-download everything a project needs, for example in a Docker build step, then build offline:

```dockerfile
COPY kargo.toml kargo.lock ./
RUN kargo toolchain install && kargo fetch
COPY . .
RUN kargo build --release --offline
```

To build without network access at all, [`kargo vendor --toolchains`](../offline-and-mirrors/#vendoring-dependencies) copies the dependencies, the Kotlin compiler and Kotlin/Native into the project. On a corporate network, `[toolchain-mirrors]` in `~/.kargo/config.toml` sends the JDK, GraalVM and Gradle downloads to your own server (see [Toolchain mirrors](../offline-and-mirrors/#toolchain-mirrors)).

`kargo fetch` also downloads the JDK that `[toolchain] jdk` pins, the JUnit launcher for `kargo test`, the JMH libraries for `kargo bench` in a package with benchmarks, and the npm packages of Wasm dependencies. Add `--tools` to fetch the documentation and formatting tools too (see [`kargo fetch`](../../reference/commands/#kargo-fetch)).
