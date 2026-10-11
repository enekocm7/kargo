---
title: Introduction
description: What Kargo is, what it does, and how it differs from Gradle.
---

**Kargo is Cargo for Kotlin.** It is a build tool and package manager that creates, builds, runs and tests Kotlin projects from a single `kargo.toml` file.

```toml title="kargo.toml"
[package]
name = "hello"
version = "0.1.0"
group = "com.example"

[kotlin]
version = "2.4.21"

[dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
```

That is a complete project. There is no `build.gradle.kts`, no `settings.gradle.kts`, no wrapper and no plugin block.

## What you get

- **Four targets.** JVM, WebAssembly (`js` or `wasi`), Kotlin/Native (Linux, macOS, Windows, iOS and more) and Android. One project can enable several of them and share code between them with `expect`/`actual`.
- **Maven dependencies.** Kargo resolves from Maven Central (plus any repositories you add) and understands POM inheritance, BOMs, exclusions, version ranges, relocations and Gradle module metadata. Kotlin Multiplatform libraries resolve to the right `.jar` or `.klib` for each target.
- **Reproducible builds.** `kargo.lock` pins every version and the SHA-256 of every downloaded file.
- **Incremental compilation.** If sources, classpath and flags have not changed, nothing is recompiled.
- **Built-in frameworks.** Compose, Ktor, Spring Boot, kotlinx.serialization, coroutines and Lombok each take one line to enable. Kargo adds the libraries, compiler plugins and flags.
- **Managed toolchains.** Kargo can download a JDK for you, and it installs Kotlin/Native automatically.
- **Extensibility.** Tasks, build hooks, compiler plugins and `kargo-<name>` subcommands.

## How it works

For the JVM, Wasm and Native targets **Kargo does not use Gradle**. It downloads the Kotlin compiler (`kotlin-compiler-embeddable`) and Kotlin/Native (`kotlin-native-prebuilt`) from Maven Central and calls them directly. That keeps builds fast and keeps everything in one config file.

**Android** is the one exception. Android builds need the Android Gradle Plugin, so Kargo generates a Gradle project under `target/android` from your `kargo.toml` and runs it for you. You still never edit Gradle files.

## Who it is for

- **People new to Kotlin** who want to write code without learning a build system first. Start with [Your first Kotlin app](../../guides/first-app/).
- **Rust developers** who already know Cargo's workflow. See [For Rust developers](../../migration/from-cargo/).
- **Gradle and Maven users** who want something simpler for apps, services, CLIs and libraries. See the [migration guides](../../migration/from-gradle/).
