---
title: Environment variables
description: Environment variables Kargo reads and sets.
---

## Variables Kargo reads

| Variable | Purpose |
|---|---|
| `KARGO_HOME` | Where Kargo keeps its cache, toolchains and `bin/` (default `~/.kargo`) |
| `KARGO_TARGET_DIR` | Build output directory (default `target/` next to the project's or workspace's `kargo.toml`) |
| `KARGO_OFFLINE` | Same as `--offline` |
| `KARGO_COMPILER_JVM_OPTS` | Extra JVM options for the Kotlin compiler daemon (or compiler process), e.g. `-Xmx4g` |
| `KARGO_DAEMON` | `false` (or `0`, `no`, `off`) runs the Kotlin compiler as a separate process on every build instead of in the [compiler daemon](../../guides/build-performance/#the-compiler-daemon) |
| `KONAN_DATA_DIR` | Kotlin/Native dependency cache (default `~/.kargo/toolchains/konan`) |
| `JAVA_HOME` | JDK to use when `[toolchain] jdk` is `system` |
| `ANDROID_HOME`, `ANDROID_SDK_ROOT` | Android SDK location |
| `BROWSER` | Browser that `kargo doc --open` starts (default: the system's default browser) |
| Names in `username-env` / `password-env` | Repository credentials |
| `COMPLETE` | Set by the `kargo completions` scripts when they ask Kargo for candidates; don't set it yourself |

## Variables Kargo sets

For tasks, hook plugins and `kargo-*` subcommands:

| Variable | Value |
|---|---|
| `KARGO_MANIFEST_DIR` | The project root |
| `KARGO_PKG_NAME`, `KARGO_PKG_VERSION`, `KARGO_PKG_GROUP` | From `[package]` |
| `KARGO_PROFILE` | `dev` or `release` |
| `KARGO_TARGET_DIR` | The output directory |
| `KARGO_KOTLIN_VERSION` | From `[kotlin]` |
| `KARGO_TASK` | The running task's name (tasks only) |
| `KARGO_HOOK` | The hook being run (hooks only) |
| `KARGO_CLASSPATH`, `KARGO_JVM_JAR` | After a JVM build |
| `KARGO_PACKAGE` | After `kargo package` |
| `KARGO_TEST_RESULT` | After tests |
