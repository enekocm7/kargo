---
title: Environment variables
description: Environment variables Kargo reads and sets.
---

Variables you set for your program, in [`[env]`](../manifest/#env) of kargo.toml or in `.env` files, are not listed here. They are described in [Environment variables](../../guides/environment/). This page lists the variables Kargo itself reads.

## Variables Kargo reads

| Variable | Purpose |
|---|---|
| `KARGO_HOME` | Where Kargo keeps its cache, toolchains and `bin/` (default `~/.kargo`) |
| `KARGO_RELEASES_URL` | Where [`kargo self update`](../commands/#kargo-self), `install.sh` and `install.ps1` look for releases (default `https://github.com/enekocm7/kargo/releases`): a mirror with the same layout, or a directory with `file://` |
| `KARGO_OSV_URL` | Where [`kargo audit`](../commands/#kargo-audit) reads security advisories (default `https://api.osv.dev`): another server with the OSV API, such as a mirror |
| `NPM_CONFIG_REGISTRY` | The npm registry [`kargo add --npm`](../commands/#npm-packages-with-kargo-add---npm), `kargo outdated` and `kargo upgrade` ask for package versions (default: the `registry` of the project's or the user's `.npmrc`, else `https://registry.npmjs.org`) |
| `NPM_CONFIG_USERCONFIG` | The user-level `.npmrc` those commands read (default `~/.npmrc`) |
| `KARGO_NO_UPDATE_CHECK` | Any non-empty value stops `kargo --version` from looking for a newer release and mentioning it |
| `KARGO_INSTALL_ROOT` | Same as `--root` of [`kargo install`](../commands/#kargo-install) and `kargo uninstall`: install below `<root>/bin` and `<root>/install` instead of `$KARGO_HOME` |
| `KARGO_TARGET_DIR` | Build output directory (default `target/` next to the project's or workspace's `kargo.toml`) |
| `KARGO_OFFLINE` | Same as `--offline` |
| `NO_COLOR` | Any non-empty value turns off the colors of Kargo's output, as `--color never` does (`--color always` still wins). `CLICOLOR_FORCE` turns them on when the output is not a terminal |
| `KARGO_VENDORED` | Same as `--vendored`: resolve dependencies only from the repository [`kargo vendor`](../commands/#kargo-vendor) wrote |
| `KARGO_REQUIRE_CHECKSUMS` | Same as `--require-checksums`: fail on a download whose repository publishes no checksum for it (see [`kargo.lock`](../lockfile/#requiring-published-checksums)) |
| `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`, `NO_PROXY` | The proxy for Kargo's downloads, and the hosts that bypass it (lowercase forms too). `[http] proxy` in `config.toml` overrides them; see [Offline and corporate builds](../../guides/offline-and-mirrors/#proxies-and-certificates) |
| `KARGO_SYSTEM_GRADLE` | Same as `--system-gradle`: Android builds use the `gradle` on `PATH` |
| `KARGO_ANDROID_DEVICE` | Same as `--device` of `kargo run -t android` and `kargo android`: the serial or virtual device name to run on, so that Kargo does not ask. Ignored by other targets. See [Devices and emulators](../../guides/android/#devices-and-emulators) |
| `KARGO_AUTO_INSTALL` | `0` stops commands from downloading the toolchains they need (a JDK when none is on `JAVA_HOME` or `PATH`, a pinned JDK or GraalVM, Kotlin/Native, Gradle, the Android SDK of `[toolchain] android-sdk`, the language servers); they fail with the `kargo toolchain install` command to run instead. `1` turns installs on over `[toolchain] auto-install = false` in `config.toml`. See [Managing JDKs](../../guides/toolchains/#what-installs-automatically) |
| `SOURCE_DATE_EPOCH` | The time in seconds since 1970 that the [`build-config`](../../guides/code-generation/#buildconfig) generator writes as `BUILD_TIME` when `build-time = true`, instead of the current time, so that builds can be reproducible |
| `KARGO_COMPILER_JVM_OPTS` | Extra JVM options for the Kotlin compiler daemon (or compiler process), e.g. `-Xmx4g` |
| `KARGO_JOBS` | Same as `-j, --jobs` of the commands that build: how many workspace members and targets build at the same time, a number or `auto` (`0`). Overrides `[build] jobs` of `kargo.toml` and `config.toml`. See [Limiting parallelism](../../guides/build-performance/#limiting-parallelism) |
| `KARGO_INCREMENTAL` | `false` (or `0`, `no`, `off`) recompiles whole targets instead of only the files a change affects ([incremental compilation](../../guides/build-performance/#incremental-compilation)) |
| `KARGO_DAEMON` | `false` (or `0`, `no`, `off`) runs the Kotlin compiler as a separate process on every build instead of in the [compiler daemon](../../guides/build-performance/#the-compiler-daemon) |
| `KARGO_NATIVE_CACHE` | `false` (or `0`, `no`, `off`) compiles debug Kotlin/Native builds without the precompiled [standard library cache](../../guides/native/#build-speed) |
| `KONAN_DATA_DIR` | Kotlin/Native dependency cache (default `~/.kargo/toolchains/konan`) |
| `JAVA_HOME` | JDK to use when `[toolchain] jdk` is `system` |
| `GRAALVM_HOME` | GraalVM to use when `[toolchain] graalvm` is `system` (the default): for `kargo package --format native-image` and `kargo run --native-agent`. Without it, a `JAVA_HOME` or `PATH` that has `native-image` |
| `ANDROID_HOME`, `ANDROID_SDK_ROOT` | Android SDK location, in that order, when [`[toolchain] android-sdk`](../manifest/#toolchain) is `system` (the default); without either, Android Studio's default location. With an API level there, Kargo uses its own SDK in `~/.kargo/toolchains/android-sdk` instead, and sets `ANDROID_HOME` to it for Gradle |
| `BROWSER` | Browser that `kargo doc --open` and `kargo run --open` start (default: the system's default browser) |
| `KARGO_BROWSER` | Browser that runs [Wasm](../../guides/wasm/#testing-in-a-browser) and [JS](../../guides/js/#browser-apps) browser tests: the path of an executable, or a command on `PATH`. Default: Chrome, Chromium, Edge, Brave or Firefox, whichever Kargo finds first |
| `KARGO_BROWSER_ARGS` | Extra arguments for that browser, separated by spaces, e.g. `--no-sandbox` for Chrome in a container |
| `KARGO_BROWSER_TIMEOUT` | Seconds a browser running Wasm or JS tests may stay silent before they count as hung (default `120`) |
| Names in `username-env` / `password-env` | Repository credentials, for downloads and for `kargo publish` |
| `KARGO_MAVEN_LOCAL` | The local Maven repository that `kargo publish --local` writes and `"maven-local"` in `[repositories]` reads (default `~/.m2/repository`) |
| `KARGO_MAVEN_CENTRAL_USERNAME`, `KARGO_MAVEN_CENTRAL_PASSWORD` | The two parts of a Central Portal user token, for [publishing to Maven Central](../../guides/publishing/#maven-central) |
| `KARGO_MAVEN_CENTRAL_URL` | The Central Portal to publish to (default `https://central.sonatype.com`) |
| `NPM_TOKEN` | The access token `kargo publish --npm` publishes with. Kargo writes it to a temporary `.npmrc` (readable by its owner only) for the upload and removes it afterwards; without it, your own `.npmrc` is used. See [Publishing to npm](../../guides/publishing/#publishing-to-npm) |
| `KARGO_MAVEN_SEARCH_URL` | The Maven Central search API that [`kargo lsp` completes coordinates from](../../guides/editors/#dependency-completion-and-update-hints) (default `https://search.maven.org/solrsearch/select`) |
| `KARGO_SIGNING_KEY` | An ASCII-armored secret key (`gpg --export-secret-keys --armor <id>`), as is or base64-encoded, that `kargo publish` [signs with](../../guides/publishing/#signing) in memory, without GnuPG. Without it, GnuPG's own keyring is used |
| `KARGO_SIGNING_KEY_ID` | The key to sign with, by fingerprint or key ID (`gpg --local-user` with GnuPG); default: the newest signing subkey, else the primary key (GnuPG's default key without `KARGO_SIGNING_KEY`) |
| `KARGO_SIGNING_KEY_PASSPHRASE`, `KARGO_SIGNING_PASSWORD` | The signing key's passphrase |
| `KARGO_GPG` | The `gpg` executable (default: `gpg` on `PATH`). With `KARGO_SIGNING_KEY`, makes GnuPG sign instead of Kargo |
| `GITHUB_ACTOR`, `GITHUB_TOKEN` | Credentials of a [GitHub Packages](../manifest/#hosted-repositories) repository without `username-env` and `password-env` |
| `CI_JOB_TOKEN` | Credentials (user `gitlab-ci-token`) of a [GitLab](../manifest/#hosted-repositories) package registry without `username-env` and `password-env` |
| `CODEARTIFACT_AUTH_TOKEN` | Credentials (user `aws`) of an [AWS CodeArtifact](../manifest/#hosted-repositories) repository without `username-env` and `password-env` |
| `COMPLETE` | Set by the `kargo completions` scripts when they ask Kargo for candidates; don't set it yourself |

## Variables Kargo sets

For tasks, hook plugins and `kargo-*` subcommands:

| Variable | Value |
|---|---|
| `KARGO_MANIFEST_DIR` | The project root (the workspace root for a `[workspace.tasks]` task run at the root) |
| `KARGO_WORKSPACE_DIR` | The workspace root, in a workspace |
| `KARGO_PKG_NAME`, `KARGO_PKG_VERSION`, `KARGO_PKG_GROUP` | From `[package]` |
| `KARGO_PROFILE` | The profile: `dev`, `release` or a custom profile's name (`--profile staging`) |
| `KARGO_TARGET_DIR` | The output directory |
| `KARGO_KOTLIN_VERSION` | From `[kotlin]` |
| `KARGO_TASK` | The running task's name (tasks only) |
| `KARGO_GEN_DIR` | The output directory of a [`[[generate]]` step](../../guides/code-generation/), `target/generated/<profile>/<name>/` (generator steps only) |
| `KARGO_GEN_ROOT`, `KARGO_GEN_NAME`, `KARGO_GEN_KIND` | The directory that holds the output of every step, the step's name, and its `kind`: `sources`, `test-sources` or `resources` (generator steps only) |
| `KARGO_HOOK` | The hook being run (hooks only) |
| `KARGO_BUILD_LOCK` | The build-directory lock this Kargo process holds (hooks only); a `kargo` command the hook runs on the same project uses that lock instead of waiting for it |
| `KARGO_CLASSPATH`, `KARGO_JVM_JAR` | After a JVM build |
| `KARGO_PACKAGE` | After `kargo package`: the fat jar, or a library's jar; with `-t android`, the first App Bundle (or AAR for a library) |
| `KARGO_TEST_RESULT` | After tests |
