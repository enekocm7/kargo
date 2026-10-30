---
title: Publishing a library
description: Publish a Kotlin library to the local Maven repository, a company repository, Maven Central or, for Wasm and JS libraries, npm with kargo publish.
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

`--dry-run` builds, stages and signs everything in `target/release/publish/repository` (a Maven repository layout you can inspect), prints each module's coordinates with its files and their SHA-256 (`Staged com.example:mathlib:1.0.0`, then `mathlib-1.0.0.jar  sha256:...` and so on), and stops before uploading.

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

A release is never overwritten. Before uploading a module, Kargo compares its files with the ones already in the repository (by their `.sha1`): when the same version is there with the same contents, the module is skipped, so a CI job that runs twice succeeds; when the contents differ, `kargo publish` fails and asks you to bump `version`. Signatures are not compared, since every run signs anew. The local Maven repository is overwritten, like `mvn install` does.

### GitHub Packages, GitLab and CodeArtifact

These services need no `username-env` and `password-env` in CI: Kargo recognizes their URLs and reads the variables their CI sets (see [Hosted repositories](../../reference/manifest/#hosted-repositories)):

```toml title="kargo.toml"
[repositories]
github = "https://maven.pkg.github.com/example/mathlib"           # GITHUB_ACTOR and GITHUB_TOKEN
gitlab = "https://gitlab.com/api/v4/projects/42/packages/maven"   # CI_JOB_TOKEN
codeartifact = "https://acme-123456789012.d.codeartifact.eu-west-1.amazonaws.com/maven/releases/"   # CODEARTIFACT_AUTH_TOKEN
```

In GitHub Actions, give the job `permissions: packages: write`. For CodeArtifact, fetch a token first:

```sh
export CODEARTIFACT_AUTH_TOKEN=$(aws codeartifact get-authorization-token --domain acme --domain-owner 123456789012 --query authorizationToken --output text)
```

Set `username-env` and `password-env` to use other credentials, such as a personal access token on your machine. Kargo doesn't upload to `s3://` buckets; put a Maven repository server in front of them, or publish to a `file:` directory and sync it.

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

With Wasm, JS or Native targets, or several targets, Kargo publishes the library the way the Kotlin Gradle Plugin does:

```text
com/example/mathlib/1.0.0/          # root module: .module, .pom, common sources
com/example/mathlib-jvm/1.0.0/      # mathlib-jvm-1.0.0.jar
com/example/mathlib-wasm-js/1.0.0/  # mathlib-wasm-js-1.0.0.klib
com/example/mathlib-js/1.0.0/       # mathlib-js-1.0.0.klib (Kotlin/JS)
com/example/mathlib-linuxx64/1.0.0/ # mathlib-linuxx64-1.0.0.klib, cinterop klibs
com/example/mathlib-android/1.0.0/  # mathlib-android-1.0.0.aar, with [target.android] kind = "library"
```

Consumers depend on the root module, `com.example:mathlib`. Its [Gradle module metadata](https://docs.gradle.org/current/userguide/publishing_gradle_module_metadata.html) (`.module`) sends each platform to its own module, so Kargo and Gradle pick the jar or klib that matches the target they build. Each platform module has its own sources jar (`commonMain/` and the platform's directory), javadoc jar, POM and `.module`. [cinterop](../native/#using-c-libraries) bindings are published next to the klib with the classifier `cinterop-<name>`. A Wasm library's [web resources](../wasm/#web-resources-from-libraries) go next to its klib as a zip with the classifier `web-resources`, announced by the `kargo.web-resources` and `kargo.web-init` properties of its POM.

### Android libraries

An Android library (`[target.android]` with `kind = "library"`) is published as the AAR of its release build, which Gradle assembles: classes, resources, its `AndroidManifest.xml`, assets, its `consumer-proguard-files` and JNI libraries. A library with only the Android target is one module, like AGP publishes one:

```text
com/example/ui/1.0.0/
├── ui-1.0.0.aar
├── ui-1.0.0-sources.jar          # src/ and src-android/
├── ui-1.0.0-javadoc.jar
├── ui-1.0.0.module               # releaseApiElements and releaseRuntimeElements
└── ui-1.0.0.pom                  # <packaging>aar</packaging>
```

With more targets, it is the `<name>-android` module of a [multiplatform library](#multiplatform-libraries). Either way, the `.module` describes the release variant with the attributes AGP and the Kotlin Gradle Plugin use (`com.android.build.api.attributes.BuildTypeAttr = release`, `org.jetbrains.kotlin.platform.type = androidJvm`), so Gradle builds of Android apps pick the AAR, and a JVM consumer of a multiplatform library keeps its jar.

Gradle resolves Android dependencies, not Kargo, so the POM lists the `[dependencies]` and `[target.android.dependencies]` with the versions `kargo.toml` declares. A dependency without a version (one a BOM manages) is left out with a warning. Path dependencies are published under their own coordinates: `<name>-android` for a multiplatform library with an Android target, its JVM module otherwise. An Android application is not published (`kargo package` builds its APK and App Bundle), nor yet a library with product flavors.

## BOMs

A bill of materials (BOM) is a POM that only pins versions, so the users of a family of libraries can align them all at once. Make a package a BOM with `kind = "bom"` in `[publish]`; its `[dependencies]` and path dependencies are the versions it pins:

```toml title="bom/kargo.toml"
[package]
name = "mathlib-bom"
version = "1.0.0"
group = "com.example"

[dependencies]
mathlib = { path = "../mathlib" }
mathlib-geometry = { path = "../geometry" }
"com.squareup.okio:okio" = "3.16.0"
"io.netty:netty-bom" = { version = "4.2.7.Final", bom = true }

[publish]
kind = "bom"
```

`kargo publish` compiles nothing for a BOM: it writes `mathlib-bom-1.0.0.pom` with `<packaging>pom</packaging>` and a `<dependencyManagement>` section listing each library at its version, each path dependency's modules (the root module and, for a [multiplatform library](#multiplatform-libraries), every platform module) at that package's version, and each `bom = true` entry as an import. Every dependency needs an explicit version.

Consumers import it like any other BOM: `{ version = "1.0.0", bom = true }` in Kargo, `platform("com.example:mathlib-bom:1.0.0")` in Gradle, and `<scope>import</scope>` in Maven. In a workspace, `kargo publish --workspace` publishes the libraries first and the BOM after them.

## Renamed artifacts

When a library changes its `group` or `name`, list its former coordinates in `relocations`:

```toml title="kargo.toml"
[package]
name = "mathlib"
version = "2.0.0"
group = "com.example"

[publish]
relocations = ["io.github.ada:mathlib"]
```

Each one gets a relocation POM at the same version (`io/github/ada/mathlib/2.0.0/mathlib-2.0.0.pom`), published with the library, that sends Maven, Gradle and Kargo to `com.example:mathlib`. Users of the old coordinates who move to 2.0.0 get the new library without changing their build. A relocation points at the package's root module.

## Shaded libraries

With [`[package.shade]`](../distributing/#shading) in `kargo.toml`, the library jar carries its dependencies, with the `relocate` packages moved, and the POM and Gradle metadata drop the dependencies the jar bundles. Dependencies you want consumers to bring (`exclude = ["org.slf4j.*"]` leaves their classes out) stay in the POM.

## JVM shrinking and obfuscation

For JVM jars, `kargo package --release` runs R8 after building the jar. Use `kargo package --optimize` to opt in with another profile. R8 can shrink, optimize and obfuscate independently through [`[package.optimize]`](../../reference/manifest/#packageoptimize). It preserves the selected main class and Kotlin metadata, reads optional ProGuard-compatible rules from `proguard-rules.pro`, and writes the minimized jar plus mapping and usage files to `target/<profile>/jvm/`.

A release package therefore leaves two jars there, and `kargo package` prints the path of the second:

| Jar | Contents |
|---|---|
| `<name>-<version>-all.jar` | The fat jar as built, not shrunk |
| `<name>-<version>-min.jar` | The same jar after R8, with your rules applied |

Run the `-min.jar` before you ship it: R8 only keeps what the built-in rules and your `proguard-rules.pro` say it reaches, so a class loaded by reflection or a service file can be missing. If it fails, add keep rules, or ship the `-all.jar`.

```toml
[package.optimize]
shrink = true
optimize = true
obfuscate = true
rules = "proguard-rules.pro"
```

Add keep rules for reflective entry points. The mapping file is needed to retrace obfuscated stack traces. Compose Desktop release jars use the same optimization.

Native platforms are published for the `platforms` of `[target.native]`, and each one has to be built on a machine that can compile for it (Apple targets on macOS).

:::caution
Kargo doesn't publish the common metadata variant yet. Gradle multiplatform projects can use a Kargo-published library from their platform source sets (`jvmMain`, `androidMain`, `wasmJsMain`, `jsMain`, ...), but not from `commonMain`.
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

Maven Central requires an [OpenPGP](https://central.sonatype.org/publish/requirements/gpg/) signature for every file. Signing is on for Maven Central and off elsewhere; `sign = true` or `sign = false` in `[publish]` overrides that.

On CI, pass the key in environment variables, and Kargo signs in memory, with no GnuPG installation (like Gradle's in-memory keys):

| Variable | Value |
|---|---|
| `KARGO_SIGNING_KEY` | The secret key, exported with `gpg --export-secret-keys --armor <KEY_ID>` (as is, or base64-encoded) |
| `KARGO_SIGNING_KEY_PASSPHRASE` | Its passphrase, if it has one (`KARGO_SIGNING_PASSWORD` works too) |
| `KARGO_SIGNING_KEY_ID` | The key to sign with, by fingerprint or key ID, when it isn't the newest signing subkey (or the primary key, without one) |

Without `KARGO_SIGNING_KEY`, Kargo signs with [GnuPG](https://gnupg.org/download/): your default key, whose passphrase GnuPG asks for. Set `KARGO_GPG` to the `gpg` executable to have GnuPG sign with `KARGO_SIGNING_KEY` too; Kargo then imports it into a temporary GnuPG home that it deletes afterwards, so your own keyring is untouched.

Remember to publish the public key to a key server (`gpg --keyserver keyserver.ubuntu.com --send-keys <KEY_ID>`); Maven Central checks signatures against it.

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

Kargo checks the requirements before it builds anything: `description`, `license`, `authors` and `repository` in `[package]`, a version that isn't a SNAPSHOT, signing, and sources and javadoc jars (except for a [BOM](#boms), which is only a POM). It uploads all modules as one bundle and waits until the Portal has validated them, printing the Portal's errors if it rejects them.

By default the deployment then waits in the Portal until you press **Publish**, so you can check it first. With `central-publishing = "automatic"` in `[publish]`, it is released as soon as it is valid.

:::caution
A release on Maven Central is permanent: it can't be changed or deleted. Try `kargo publish --dry-run` and the manual mode first.
:::

## Publishing to npm

A Wasm or Kotlin/JS library (`kind = "library"` in [`[target.wasm]`](../wasm/#libraries-and-npm-packages), with `environment = "js"` or `"browser"`, or in [`[target.js]`](../js/#libraries-and-npm-packages)) is an npm package, and `kargo publish --npm` uploads it with npm, pnpm or bun, whichever the project uses (the `package-manager` of the target, else the first of the three on `PATH`):

```toml title="kargo.toml"
[package]
name = "greeter"
version = "0.3.0"
description = "Greets people"
license = "MIT"
repository = "https://github.com/acme/greeter"

[target.wasm]
environment = "browser"
kind = "library"

[publish.npm]
scope = "acme"        # publishes @acme/greeter
access = "public"
```

```console
$ kargo publish --npm --dry-run
   Compiling greeter v0.3.0 (wasm)
   Packaging @acme/greeter v0.3.0 (npm)
      Packed 5 files, 4.1 KiB (target/release/npm/acme-greeter/acme-greeter-0.3.0.tgz)
             greeter.d.mts  412 B
             greeter.mjs  2.3 KiB
             greeter.wasm  1.6 KiB
             package.json  1.1 KiB
             ...
warning: aborting upload of @acme/greeter to https://registry.npmjs.org due to dry run
$ NPM_TOKEN=... kargo publish --npm
   Uploading @acme/greeter v0.3.0 to https://registry.npmjs.org
   Published @acme/greeter v0.3.0 to https://registry.npmjs.org
```

`--dry-run` builds and packs the package and lists the files and their sizes, but uploads nothing. What it lists is what a real publish uploads: Kargo packs once and publishes that tarball, which it keeps in `target/release/npm/`.

### Checks

Kargo checks what it can before it compiles anything, and fails with what to change:

- `version` is a SemVer version (`1.2.3`; not `1.0-SNAPSHOT`), and a pre-release (`1.2.3-beta.1`) is published under a dist-tag, with `--tag next` or `tag` in `[publish.npm]`, so it does not become `latest`.
- The name is a valid npm name (lowercase, URL-safe, scoped like `@scope/name` or made so by `scope`).
- The registry does not have this version yet. A published version never changes; bump `version`. `--dry-run` runs this check too, so it fails on a version that is already published, which tells you before the real publish does. A dry run only warns when the registry can't be reached, and with `--offline` a dry run skips the check (a real publish refuses to run offline).
- With `provenance = true`: the package manager is npm or pnpm, not bun, and the job can sign (see [Provenance](#provenance)).

After the build it checks that the `package.json` is the one of this version, that the files it names as `main`, `module` and `types` exist and that every pattern in `files` matches a file. A package without a `description`, `license` or `repository` still publishes, with a [warning](../../reference/warnings/#npm-packages-without-metadata).

### `package.json`

Kargo writes the `package.json` next to the compiled module on every build, so `npm publish target/release/wasm` works by hand too. Besides the module, its declarations and the npm packages it needs, it holds what `[package]` and `[publish.npm]` say:

| `package.json` | From |
|---|---|
| `name` | `npm-name` of the target, else the lowercase package name, with `scope` of `[publish.npm]` when it has none |
| `version`, `description`, `license`, `homepage` | `[package]` |
| `author`, `contributors` | the first and the other `authors` |
| `repository` | `[package] repository`, as `{ "type": "git", "url": "git+https://....git" }` |
| `bugs` | `bugs` of `[publish.npm]`, else the issues page of a GitHub or GitLab repository |
| `keywords`, `sideEffects`, `engines` | `keywords`, `side-effects` and `engines` of `[publish.npm]` |
| `main`, `module`, `types`, `exports` | the compiled module. `exports` lists the conditions `types`, `import` (`require` for CommonJS and UMD) and `default`, in the order TypeScript reads them |
| `files` | the module, its `.wasm` and its declarations |

### Credentials

Credentials are never read from `kargo.toml`. Kargo takes them from:

1. **`NPM_TOKEN`**, an access token that can publish. Kargo writes it to a temporary `.npmrc` in the package's directory (readable by its owner only on Linux and macOS) with the registry, runs the package manager and deletes the file, also when the upload fails. The token is not in the command line or in Kargo's output.
2. **Your own `.npmrc`** (`~/.npmrc` or the project's) when `NPM_TOKEN` is not set, as with a plain `npm publish`. `npm login` works.

When neither gives a token for the registry, Kargo prints a warning and still tries the upload instead of failing: it only reads `_authToken` entries, and other ways of logging in (such as `_auth`) are left to the package manager, which reports the real error if they don't work.

The registry is `--registry`, else `registry` in `[publish.npm]`, else the one your `.npmrc` names for the package (`registry=` and `@scope:registry=`), else `https://registry.npmjs.org`. For GitHub Packages:

```toml title="kargo.toml"
[publish.npm]
registry = "https://npm.pkg.github.com"
scope = "acme"           # GitHub requires the owner as the scope
```

A registry that asks for a one-time password (two-factor authentication) fails with a hint; pass `--otp <code>`. For scoped packages npm defaults to `access = "restricted"`, which needs a paid account, so set `access = "public"` for open source.

### Provenance

`provenance = true` publishes with [npm provenance](https://docs.npmjs.com/generating-provenance-statements): the registry shows that the package was built from your repository by a CI run. It needs npm or pnpm: bun can't publish with provenance, so Kargo refuses `provenance = true` when the package manager is bun (set `package-manager = "npm"` or `"pnpm"` under the target, or turn provenance off). It also needs a job that can sign: on GitHub Actions give it the `id-token: write` permission, and on GitLab CI a `SIGSTORE_ID_TOKEN`. Kargo fails before building when the job is missing it. A dry run does not check it. See [Publishing from CI](#publishing-from-ci).

### Maven and npm together

With `[publish.npm]` set, `kargo publish` publishes the Maven modules first (when a `repository` is given or set in `[publish]`) and then the npm package. `kargo publish --npm` and `kargo publish --maven` publish to one only. A library that has only an npm destination needs no `group`.

The klib that Maven consumers use (`greeter-wasm-js`) and the npm package are different artifacts of the same library, so a Kotlin project depends on the first and a JavaScript project on the second.

## Workspaces

`kargo publish --workspace` (or `kargo publish` at the root of a workspace) publishes every member (`--exclude <name>` leaves one out), path dependencies before the packages that use them. Members with `publish = false` in `[package]` are skipped, which is how you keep applications and internal tools out of the repository:

```toml title="app/kargo.toml"
[package]
name = "app"
version = "0.1.0"
publish = false
```

With `--npm`, the members that build no npm package are skipped, and an npm package is published after the ones it refers to. A member refers to another when its `[target.wasm.npm-dependencies]` (or `[target.js.npm-dependencies]`) names it with `workspace:*` (the exact version; `workspace:^` and `workspace:~` keep their prefix) or with a `file:` or `link:` path into that member's directory. Kargo writes the other package's version into the published `package.json`, since a registry can't install a path:

```toml title="ui/kargo.toml"
[target.wasm.npm-dependencies]
"@acme/core" = "workspace:*"      # published as "@acme/core": "2.1.0"
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

Kargo signs with `SIGNING_KEY` itself, so the job needs no GnuPG.

To publish an npm package from CI, give the job the token, and the `id-token` permission when `provenance = true`:

```yaml title=".github/workflows/publish-npm.yml"
name: Publish to npm
on:
  push:
    tags: ["v*"]

permissions:
  contents: read
  id-token: write        # for provenance = true

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 22
      - name: Install Kargo
        run: |
          curl -fsSL https://enekocm7.github.io/kargo/install.sh | sh -s -- --no-modify-path
          echo "$HOME/.kargo/bin" >> "$GITHUB_PATH"
      - run: kargo publish --locked --npm
        env:
          NPM_TOKEN: ${{ secrets.NPM_TOKEN }}
```

A re-run for a tag that was published fails with `is already on the registry` before anything is built. When the job publishes to a repository of your own rather than Maven Central, running it again for the same tag skips the modules already uploaded. See [Continuous integration](../ci/) for caching.
