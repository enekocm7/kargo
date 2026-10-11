---
title: Managing JDKs
description: Pin a JDK per project and manage installed JDKs and Kotlin/Native distributions.
---

Kargo can use the JDK you already have, or download and manage JDKs for you, per project.

## Pinning a JDK

```toml title="kargo.toml"
[toolchain]
jdk = "21"              # the newest Temurin 21
# jdk = "21.0.5+11"     # an exact release
# jdk = "system"        # the default: JAVA_HOME, then `java` on PATH
```

With anything other than `system`, Kargo downloads that [Eclipse Temurin](https://adoptium.net) JDK from Adoptium the first time it's needed, verifies the published SHA-256 and installs it under `~/.kargo/toolchains/jdk/<release>`. That JDK is then used for compiling, running, testing and for Android's Gradle build.

A major version like `"21"` uses the newest release of 21 you have installed, and downloads one if you have none.

Kargo also checks that `[target.jvm] jvm-target` is not newer than the pinned JDK, so you get a clear error instead of a confusing compiler failure.

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

## The `kargo toolchain` command

```console
$ kargo toolchain available          # JDK versions you can install (LTS releases are marked)
$ kargo toolchain install jdk 21     # newest 21
$ kargo toolchain install jdk 21.0.5+11
$ kargo toolchain install native 2.4.21
$ kargo toolchain install            # everything the current project needs
$ kargo toolchain list               # installed toolchains, disk usage, and what this project uses
$ kargo toolchain which              # the toolchains this project resolves to
$ kargo toolchain remove jdk 17      # every JDK 17 release
$ kargo toolchain remove jdk 17.0.13+11
$ kargo toolchain remove native 2.4.10
```

`kargo toolchain ls` and `kargo toolchain rm` are aliases.

## Kotlin/Native

Kotlin/Native distributions are tied to `[kotlin] version` and installed automatically on the first native build, under `~/.kargo/toolchains/kotlin-native/<version>`. Their LLVM toolchains and sysroots go in `~/.kargo/toolchains/konan` (override with `KONAN_DATA_DIR`).

## The Kotlin compiler

You don't install the Kotlin compiler. Kargo downloads the version in `[kotlin] version` from Maven Central into its cache. To try a new Kotlin release, change the version and rebuild.

## Offline and Docker

Pre-download everything a project needs, for example in a Docker build step, then build offline:

```dockerfile
COPY kargo.toml kargo.lock ./
RUN kargo toolchain install && kargo fetch
COPY . .
RUN kargo build --release --offline
```
