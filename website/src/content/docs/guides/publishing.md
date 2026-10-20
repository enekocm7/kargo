---
title: Publishing a library
description: Publish a Kotlin library to the local Maven repository, a company repository or Maven Central with kargo publish.
---

`kargo publish` builds a library with the release profile and publishes it to a Maven repository, the way `cargo publish` sends a crate to crates.io. What it uploads is what Maven and Gradle expect, so people who use your library need neither Kargo nor any setup beyond the dependency itself.

```toml title="kargo.toml"
[package]
name = "mathlib"
version = "1.0.0"
group = "com.example"
```

```console
$ kargo publish --local
   Compiling mathlib v1.0.0 (jvm)
   Packaging mathlib v1.0.0 (jvm)
 Documenting mathlib v1.0.0 (jvm)
   Uploading com.example:mathlib v1.0.0 (20 files)
   Published com.example:mathlib:1.0.0 to maven-local (/home/me/.m2/repository)
    Finished `release` profile [optimized] target(s) in 9.84s
```

Publishing needs a `group` in `[package]`: together with `name` and `version` it makes the coordinates `com.example:mathlib:1.0.0`.

## Where to publish

| Destination | Command | |
|---|---|---|
| The local Maven repository | `kargo publish --local` | `~/.m2/repository`, like `mvn install` |
| A repository you run | `kargo publish --repository <name>` | A [`[repositories]`](../../reference/manifest/#repositories) entry: Nexus, Artifactory, GitHub Packages, Reposilite, or a directory |
| Maven Central | `kargo publish --repository maven-central` | Through the [Central Portal](#maven-central) |

Set a default in `[publish]` so a plain `kargo publish` knows where to go:

```toml title="kargo.toml"
[publish]
repository = "maven-central"
```

`--dry-run` builds, stages and signs everything in `target/release/publish/repository` (a Maven repository layout you can inspect) and stops before uploading.

### The local Maven repository

`kargo publish --local` makes a library available to every project on your machine right away. Another Kargo project reads it with the `maven-local` repository:

```toml title="kargo.toml"
[repositories]
local = "maven-local"

[dependencies]
"com.example:mathlib" = "1.0.0"
```

Maven and Gradle projects use `mavenLocal()` as usual. Set `KARGO_MAVEN_LOCAL` to use another directory.

### Your own repository

Any `[repositories]` entry can be published to, with the same credentials Kargo downloads with:

```toml title="kargo.toml"
[repositories]
company = { url = "https://repo.example.com/maven-releases", username-env = "REPO_USER", password-env = "REPO_PASS" }

[publish]
repository = "company"
```

Kargo uploads every file with an HTTP `PUT`, like `mvn deploy`, and adds the version to the module's `maven-metadata.xml`. A `file:` URL is written as a directory. A SNAPSHOT version keeps its plain `-SNAPSHOT` file names and is overwritten by the next publish.

:::note[GitHub Packages]
Use `https://maven.pkg.github.com/<owner>/<repository>` as the URL, your user name as the username and a token with the `write:packages` scope as the password.
:::

## What gets published

A library with only the JVM target is one Maven module:

```text
com/example/mathlib/1.0.0/
├── mathlib-1.0.0.jar            # your classes only, no dependencies
├── mathlib-1.0.0-sources.jar
├── mathlib-1.0.0-javadoc.jar
└── mathlib-1.0.0.pom
```

Every file comes with its `.md5`, `.sha1`, `.sha256` and `.sha512` checksums, and with an `.asc` signature when [signing](#signing) is on.

The POM lists your dependencies (and those of your [frameworks](../frameworks/), such as `kotlin-stdlib`) at the versions `kargo.lock` resolved, BOMs as imports, and path dependencies under their own coordinates. Like Gradle's `api` and `implementation`, a dependency gets Maven's `compile` scope only when it is part of the library's API: declared with `exported = true` (or a framework's library). Every other dependency, and every `runtime` one, gets the `runtime` scope, so it is there when consumers run the library but not when they compile against it. `provided`, `processor` and `ksp` dependencies are left out:

```toml title="kargo.toml"
[dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = { version = "0.7.1", exported = true }   # compile: in the public API
"com.squareup.okio:okio" = "3.16.0"                                               # runtime: used internally
```

 The POM also carries the project information from `[package]`:

```toml title="kargo.toml"
[package]
name = "mathlib"
version = "1.0.0"
group = "com.example"
description = "Small math helpers"
license = "MIT OR Apache-2.0"
authors = ["Ada Lovelace <ada@example.com>"]
homepage = "https://example.com/mathlib"
repository = "https://github.com/example/mathlib"
```

`license` is an SPDX expression; each license links to its page on spdx.org. `authors` become developers, and `repository` becomes the source control section (`scm:git:<repository>`).

A package that is a program, with a `main` function or a `main-class`, also gets its main class in the POM as a `kargo.main-class` property. That is how `kargo install com.example:tool` knows what to run when the jar's manifest has no `Main-Class`; see [Installing command-line tools](../installing-tools/#from-maven).

### Multiplatform libraries

With Wasm or Native targets, or several targets, Kargo publishes the library the way the Kotlin Gradle Plugin does:

```text
com/example/mathlib/1.0.0/          # root module: .module, .pom, common sources
com/example/mathlib-jvm/1.0.0/      # mathlib-jvm-1.0.0.jar
com/example/mathlib-wasm-js/1.0.0/  # mathlib-wasm-js-1.0.0.klib
com/example/mathlib-linuxx64/1.0.0/ # mathlib-linuxx64-1.0.0.klib, cinterop klibs
```

Consumers depend on the root module, `com.example:mathlib`. Its [Gradle module metadata](https://docs.gradle.org/current/userguide/publishing_gradle_module_metadata.html) (`.module`) sends each platform to its own module, so Kargo and Gradle pick the jar or klib that matches the target they build. Each platform module has its own sources jar (`commonMain/` and the platform's directory), javadoc jar, POM and `.module`. [cinterop](../native/#using-c-libraries) bindings are published next to the klib with the classifier `cinterop-<name>`.

Native platforms are published for the `platforms` of `[target.native]`, and each one has to be built on a machine that can compile for it (Apple targets on macOS).

:::caution
Kargo doesn't publish the common metadata variant yet. Gradle multiplatform projects can use a Kargo-published library from their platform source sets (`jvmMain`, `wasmJsMain`, ...), but not from `commonMain`. The Android target is not published.
:::

### Sources and javadoc

The javadoc jar holds the HTML that [Dokka](../documentation/) generates from your KDoc. Dokka is downloaded on first use; `[doc]` options apply. To skip it:

```toml title="kargo.toml"
[publish]
javadoc = "empty"   # a jar with only a manifest; enough for Maven Central
sources = true      # the default
```

`javadoc = false` and `sources = false` publish no jar at all, which Maven Central doesn't accept.

## Signing

Maven Central requires an [OpenPGP](https://central.sonatype.org/publish/requirements/gpg/) signature for every file. Kargo signs with [GnuPG](https://gnupg.org/download/), so `gpg` has to be installed. Signing is on for Maven Central and off elsewhere; `sign = true` or `sign = false` in `[publish]` overrides that.

On your machine, Kargo signs with your default key and GnuPG asks for its passphrase. On CI, pass the key in environment variables instead:

| Variable | Value |
|---|---|
| `KARGO_SIGNING_KEY` | The secret key, exported with `gpg --export-secret-keys --armor <KEY_ID>` (as is, or base64-encoded) |
| `KARGO_SIGNING_KEY_PASSPHRASE` | Its passphrase, if it has one |
| `KARGO_SIGNING_KEY_ID` | The key to use, when it isn't the default one |

Kargo imports `KARGO_SIGNING_KEY` into a temporary GnuPG home that it deletes afterwards, so your own keyring is untouched. Remember to publish the public key to a key server (`gpg --keyserver keyserver.ubuntu.com --send-keys <KEY_ID>`); Maven Central checks signatures against it.

## Maven Central

Maven Central takes uploads through the [Central Portal](https://central.sonatype.com):

1. Create an account and verify the namespace that matches your `group` (`com.example` needs `example.com`, `io.github.me` your GitHub account).
2. Generate a user token on the account page and export its two parts:

   ```sh
   export KARGO_MAVEN_CENTRAL_USERNAME=<token username>
   export KARGO_MAVEN_CENTRAL_PASSWORD=<token password>
   ```

3. Fill in the POM fields Maven Central requires, and set up [signing](#signing):

   ```toml title="kargo.toml"
   [package]
   name = "mathlib"
   version = "1.0.0"
   group = "io.github.me"
   description = "Small math helpers"
   license = "Apache-2.0"
   authors = ["Ada Lovelace <ada@example.com>"]
   repository = "https://github.com/me/mathlib"

   [publish]
   repository = "maven-central"
   ```

4. Publish:

   ```console
   $ kargo publish
      Compiling mathlib v1.0.0 (jvm)
      Packaging mathlib v1.0.0 (jvm)
    Documenting mathlib v1.0.0 (jvm)
        Signing 4 files
      Uploading io.github.me:mathlib:1.0.0 to Maven Central (24 files)
     Validating deployment 28570f16-da32-4c14-bd2e-c1acc0782365
      Validated io.github.me:mathlib:1.0.0 (deployment 28570f16-da32-4c14-bd2e-c1acc0782365)
   note: release it at https://central.sonatype.com/publishing/deployments, or set `central-publishing = "automatic"` in [publish]
   ```

Kargo checks the requirements before it builds anything: `description`, `license`, `authors` and `repository` in `[package]`, a version that isn't a SNAPSHOT, signing, and sources and javadoc jars. It uploads all modules as one bundle and waits until the Portal has validated them, printing the Portal's errors if it rejects them.

By default the deployment then waits in the Portal until you press **Publish**, so you can check it first. With `central-publishing = "automatic"` in `[publish]`, it is released as soon as it is valid.

:::caution
A release on Maven Central is permanent: it can't be changed or deleted. Try `kargo publish --dry-run` and the manual mode first.
:::

## Workspaces

`kargo publish --workspace` (or `kargo publish` at the root of a workspace) publishes every member (`--exclude <name>` leaves one out), path dependencies before the packages that use them. Members with `publish = false` in `[package]` are skipped, which is how you keep applications and internal tools out of the repository:

```toml title="app/kargo.toml"
[package]
name = "app"
version = "0.1.0"
publish = false
```

A path dependency becomes an ordinary dependency on the other package's coordinates, so it has to be published too, with a `group` of its own. Its POM scope is `compile` when the path dependency is `exported`, and `runtime` otherwise. Members can share `version`, `group`, `license` and the other `[package]` keys through [`[workspace.package]`](../workspaces/#shared-settings).

## Publishing from CI

A GitHub Actions job that publishes to Maven Central when a release tag is pushed:

```yaml title=".github/workflows/publish.yml"
name: Publish
on:
  push:
    tags: ["v*"]

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-java@v4
        with:
          distribution: temurin
          java-version: 21
      - name: Install Kargo
        run: |
          curl -fsSL https://enekocm7.github.io/kargo/install.sh | sh -s -- --no-modify-path
          echo "$HOME/.kargo/bin" >> "$GITHUB_PATH"
      - run: kargo publish --locked --repository maven-central
        env:
          KARGO_MAVEN_CENTRAL_USERNAME: ${{ secrets.MAVEN_CENTRAL_USERNAME }}
          KARGO_MAVEN_CENTRAL_PASSWORD: ${{ secrets.MAVEN_CENTRAL_PASSWORD }}
          KARGO_SIGNING_KEY: ${{ secrets.SIGNING_KEY }}
          KARGO_SIGNING_KEY_PASSPHRASE: ${{ secrets.SIGNING_KEY_PASSPHRASE }}
```

`gpg` is preinstalled on GitHub's runners. See [Continuous integration](../ci/) for caching.
