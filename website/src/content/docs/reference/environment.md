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
| `KARGO_SYSTEM_GRADLE` | Same as `--system-gradle`: Android builds use the `gradle` on `PATH` |
| `KARGO_COMPILER_JVM_OPTS` | Extra JVM options for the Kotlin compiler daemon (or compiler process), e.g. `-Xmx4g` |
| `KARGO_INCREMENTAL` | `false` (or `0`, `no`, `off`) recompiles whole targets instead of only the files a change affects ([incremental compilation](../../guides/build-performance/#incremental-compilation)) |
| `KARGO_DAEMON` | `false` (or `0`, `no`, `off`) runs the Kotlin compiler as a separate process on every build instead of in the [compiler daemon](../../guides/build-performance/#the-compiler-daemon) |
| `KARGO_NATIVE_CACHE` | `false` (or `0`, `no`, `off`) compiles debug Kotlin/Native builds without the precompiled [standard library cache](../../guides/native/#build-speed) |
| `KONAN_DATA_DIR` | Kotlin/Native dependency cache (default `~/.kargo/toolchains/konan`) |
| `JAVA_HOME` | JDK to use when `[toolchain] jdk` is `system` |
| `ANDROID_HOME`, `ANDROID_SDK_ROOT` | Android SDK location |
| `BROWSER` | Browser that `kargo doc --open` and `kargo run --open` start (default: the system's default browser) |
| `KARGO_BROWSER` | Browser that runs [Wasm browser tests](../../guides/wasm/#testing-in-a-browser): the path of an executable, or a command on `PATH`. Default: Chrome, Chromium, Edge, Brave or Firefox, whichever Kargo finds first |
| `KARGO_BROWSER_ARGS` | Extra arguments for that browser, separated by spaces, e.g. `--no-sandbox` for Chrome in a container |
| `KARGO_BROWSER_TIMEOUT` | Seconds a browser running Wasm tests may stay silent before they count as hung (default `120`) |
| Names in `username-env` / `password-env` | Repository credentials, for downloads and for `kargo publish` |
| `KARGO_MAVEN_LOCAL` | The local Maven repository that `kargo publish --local` writes and `"maven-local"` in `[repositories]` reads (default `~/.m2/repository`) |
| `KARGO_MAVEN_CENTRAL_USERNAME`, `KARGO_MAVEN_CENTRAL_PASSWORD` | The two parts of a Central Portal user token, for [publishing to Maven Central](../../guides/publishing/#maven-central) |
| `KARGO_MAVEN_CENTRAL_URL` | The Central Portal to publish to (default `https://central.sonatype.com`) |
| `KARGO_MAVEN_SEARCH_URL` | The Maven Central search API that [`kargo lsp` completes coordinates from](../../guides/editors/#dependency-completion-and-update-hints) (default `https://search.maven.org/solrsearch/select`) |
| `KARGO_SIGNING_KEY` | An ASCII-armored secret key (`gpg --export-secret-keys --armor <id>`), as is or base64-encoded, that `kargo publish` [signs with](../../guides/publishing/#signing). Without it, GnuPG's own keyring is used |
| `KARGO_SIGNING_KEY_ID` | The key to sign with (`gpg --local-user`); default: GnuPG's default key |
| `KARGO_SIGNING_KEY_PASSPHRASE` | The signing key's passphrase |
| `KARGO_GPG` | The `gpg` executable (default: `gpg` on `PATH`) |
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
| `KARGO_BUILD_LOCK` | The build-directory lock this Kargo process holds (hooks only); a `kargo` command the hook runs on the same project uses that lock instead of waiting for it |
| `KARGO_CLASSPATH`, `KARGO_JVM_JAR` | After a JVM build |
| `KARGO_PACKAGE` | After `kargo package`: the fat jar, or a library's jar |
| `KARGO_TEST_RESULT` | After tests |
