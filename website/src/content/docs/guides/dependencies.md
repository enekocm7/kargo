---
title: Managing dependencies
description: Add, remove, pin and inspect Maven dependencies, BOMs and repositories.
---

Kargo uses Maven coordinates (`group:artifact:version`) and downloads from Maven Central by default. Search for libraries on [central.sonatype.com](https://central.sonatype.com) or [klibs.io](https://klibs.io) (for Kotlin Multiplatform libraries).

## Adding dependencies

```sh
kargo add com.squareup.okio:okio                 # latest release
kargo add com.squareup.okio:okio:3.10.2          # an exact version
kargo add io.mockk:mockk --dev                   # test-only
kargo add org.slf4j:slf4j-simple --scope runtime # only needed at runtime
kargo add jakarta.servlet:jakarta.servlet-api --scope provided
kargo add com.google.auto.value:auto-value:1.11.0 --scope processor # a javac annotation processor
kargo add com.squareup.moshi:moshi-kotlin-codegen:1.15.2 --scope ksp # a KSP processor
kargo add com.google.guava:guava -t jvm          # only for the JVM target
kargo add org.jetbrains.kotlinx:kotlinx-io-core --pre   # allow a pre-release as "latest"
kargo add --npm left-pad                         # an npm package, for the wasm or js target
```

Kargo checks that the artifact exists before editing `kargo.toml` (skip that with `--no-verify`), and it keeps your file's comments and formatting.

Without a version, Kargo picks the newest release. Some libraries publish variants next to their plain versions (`0.8.0-0.6.x-compat`, `33.0.0-jre`); Kargo prefers the plain version and prints a note naming the variant it skipped. Pass the full coordinates to pick a variant.

To remove one, name the table it is in the same way as for `kargo add`:

```sh
kargo remove com.squareup.okio:okio
kargo rm io.mockk:mockk --dev
kargo rm com.google.guava:guava -t jvm
```

### npm packages

The wasm and js targets take npm packages besides Maven libraries, in `[target.wasm.npm-dependencies]` and `[target.js.npm-dependencies]`. `kargo add --npm` and `kargo remove --npm` edit those tables the way `kargo add` and `kargo remove` edit the Maven ones:

```sh
kargo add --npm left-pad                  # ^1.3.0, the registry's latest release
kargo add --npm @js-joda/core@^3.2.0      # a scoped package with a range
kargo add --npm chart.js@next --types     # a dist-tag; also @types/chart.js when it ships no declarations
kargo add --npm mocha --dev -t js         # [target.js.dev-npm-dependencies], for tests only
kargo rm --npm left-pad
```

Versions come from the npm registry of your `.npmrc` (see [`kargo add --npm`](../../reference/commands/#npm-packages-with-kargo-add---npm)); the entries are ranges, and the versions that get installed are the package manager's. See [Your own npm packages](../wasm/#your-own-npm-packages) for what Kargo does with them. `kargo audit` checks the installed versions, and `kargo tree` shows which library asked for each package.

## Keeping dependencies up to date

`kargo outdated` lists the dependencies in `kargo.toml` that have newer versions, and `kargo upgrade` raises them:

```console
$ kargo outdated
name                                    current  compatible  latest   kind
org.jetbrains.kotlinx:kotlinx-datetime  0.6.0    0.6.2       0.8.0    dependencies
io.mockk:mockk                          1.13.0   1.14.11     1.14.11  dev-dependencies
```

```sh
kargo upgrade --dry-run                  # preview
kargo upgrade                            # newest compatible versions
kargo upgrade -i io.mockk:mockk          # newest version of one dependency, even a new major version
```

A compatible version has the same major version as the current one (for `0.x`, the same minor version), like Cargo's default requirements. `kargo upgrade` edits only the version strings, so your comments and layout stay as they are. Dependencies declared with a range or managed by a BOM are skipped.

The npm packages of the wasm and js targets are listed and upgraded next to the Maven ones, against the npm registry, within the range in `kargo.toml`: `^1.0.0` moves to the newest `1.x`, and `-i` crosses to the next major version. Only `1.2.3`, `^1.2.3` and `~1.2.3` are rewritten; other ranges are left alone.

`kargo upgrade` changes what you declared in `kargo.toml`. To move versions in `kargo.lock` only, for example a transitive dependency, use `kargo update` (see [The lockfile](#the-lockfile)).

## Security advisories

`kargo audit` looks up every resolved dependency, transitive ones and the npm packages of Wasm and JS included, in the [OSV](https://osv.dev) database of security advisories:

```console
$ kargo audit
    Auditing app v0.1.0 (42 packages, advisories from https://api.osv.dev)
severity        package          version  advisory             fixed in
critical (9.8)  com.acme:widget  1.2.0    GHSA-wwww-wwww-wwww  1.2.5
...
```

Each advisory comes with its link and the path of dependencies that brings the package in, as `kargo tree` would show it. `kargo audit --fix` raises the versions you declare in `kargo.toml` to the ones that fix the advisories; for a transitive dependency, upgrade the library that brings it in, or declare the module at the fixed version yourself, which wins because the [highest version wins](#version-conflicts). An advisory that does not apply to you goes in [`[audit] ignore`](../../reference/manifest/#audit), with the reason:

```toml title="kargo.toml"
[audit]
ignore = [{ id = "CVE-2024-1234", reason = "we never parse untrusted XML", until = 2026-12-31 }]
```

The audit fails (exit code `101`) on advisories of `high` severity or worse, which makes it a CI step (see [Security advisories in CI](../ci/#security-advisories)). It is the only command that sends your dependency list anywhere: the names and versions go to `api.osv.dev`, and the answers are cached for a day. See [`kargo audit`](../../reference/commands/#kargo-audit) for the options.

## Writing dependencies by hand

All of these forms are valid in `[dependencies]`, `[dev-dependencies]` and `[target.<name>.dependencies]`:

```toml title="kargo.toml"
[dependencies]
# "group:artifact" = "version"
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"

# A short key with the group/artifact spelled out
guava = { group = "com.google.guava", version = "33.4.0-jre" }

# Or full coordinates in `coords`
logback = { coords = "ch.qos.logback:logback-classic:1.5.32", scope = "runtime" }

# Exclude transitive dependencies
"com.example:big-lib" = { version = "2.0", exclude = ["commons-logging:commons-logging"] }

# Don't pull in any transitive dependencies
"com.example:standalone" = { version = "1.0", transitive = false }

# A specific file of a module: a classifier and/or an extension (type)
"com.example:tool" = { version = "1.0", classifier = "all" }

# Version ranges use Maven syntax
"com.example:ranged" = "[1.2,2.0)"
```

A [test suite](../testing/#test-suites) has its own `[test.suites.<name>.dependencies]` with the same forms, for libraries only that suite's tests use (locked as `jvm/test-<name>` in [`kargo.lock`](../../reference/lockfile/)).

| Key | Meaning |
|---|---|
| `version` | A version or Maven range. Required unless a BOM manages it. |
| `group`, `artifact`, `coords` | Alternatives to writing `"group:artifact"` as the key. |
| `scope` | `compile` (default), `runtime`, `provided`, `processor` or `ksp`. |
| `exclude` | `group:artifact` (or `group:*`) entries to drop from this dependency's tree. |
| `transitive` | `false` to ignore this dependency's own dependencies. |
| `force` | `true` to pin this version even if something else asks for a newer one. |
| `classifier`, `extension` | Select a specific file, such as `natives-linux` or `aar` (see [Classifiers and native libraries](#classifiers-and-native-libraries)). |
| `bom` | `true` to import this POM as a BOM (see below). |

### Classifiers and native libraries

A dependency with a `classifier` or `extension` selects one file of a module, such as `lwjgl-3.3.6-natives-windows.jar`. Libraries with native code publish those files next to their main jar, so you declare the module twice: once for its classes, and once for each native classifier. TOML keys are unique, so give the second declaration a key of your own and spell out the coordinates:

```toml title="kargo.toml"
[dependencies]
"org.lwjgl:lwjgl" = "3.3.6"
lwjgl-natives = { coords = "org.lwjgl:lwjgl:3.3.6", classifier = "natives-windows", scope = "runtime" }
```

Both jars are on the runtime classpath, in the [`kargo package`](../../reference/commands/) jar and in [`kargo.lock`](../../reference/lockfile/). The natives are only needed at runtime, so `scope = "runtime"` keeps their jar off the compile classpath. Every dependency is identified by `group:artifact:classifier:extension`; declaring the same one twice is an error.

A module still has **one version**, however many classifiers you declare. If the declarations, or other libraries, ask for different versions, the highest wins (or the one with `force = true`), and every classifier uses it.

:::note
A module that you only declare with a `classifier` gets just that file, as in Gradle. Its main jar is added when you also declare the module without a classifier, or when another dependency needs it.
:::

`transitive = false` on the classifier declaration only affects that declaration: the plain one still brings in the module's own dependencies. `kargo tree` lists the module once, and `kargo tree --format json` lists every file it contributes.

### Scopes

| Scope | Compile classpath | Runtime classpath | In `kargo package` jar |
|---|---|---|---|
| `compile` | yes | yes | yes |
| `runtime` | no | yes | yes |
| `provided` | yes | no | no |
| `processor` | no (`javac -processorpath` only) | no | no |
| `ksp` | no (KSP processor path only) | no | no |

## Annotation processors

Java annotation processors such as [AutoValue](https://github.com/google/auto/tree/main/value), [MapStruct](https://mapstruct.org) or [Immutables](https://immutables.github.io) generate code while `javac` compiles your Java sources. Declare the processor with `scope = "processor"` and its annotations as a normal (usually `provided`) dependency:

```toml title="kargo.toml"
[target.jvm.dependencies]
"com.google.auto.value:auto-value-annotations" = { version = "1.11.0", scope = "provided" }
"com.google.auto.value:auto-value" = { version = "1.11.0", scope = "processor" }
```

```java title="src/Person.java"
package com.example.app;

import com.google.auto.value.AutoValue;

@AutoValue
public abstract class Person {
    public abstract String name();

    public static Person create(String name) {
        return new AutoValue_Person(name);
    }
}
```

Processor dependencies and their own dependencies go on `javac`'s `-processorpath`, and Kargo passes `-proc:full` so they run on every JDK (JDK 23 and newer no longer run processors found on the classpath). They never reach the compile or runtime classpath and are not packaged. Processors declared in `[dependencies]` or `[target.jvm.dependencies]` run for the main and test sources; a processor in `[dev-dependencies]` runs for the tests only. Like everything else, processors are pinned in `kargo.lock`. When no processor is declared, `javac` runs with `-proc:none`.

Pass processor options and other `javac` flags with `[target.jvm] javac-args`:

```toml title="kargo.toml"
[target.jvm]
javac-args = ["-Amapstruct.defaultComponentModel=spring", "-Xlint:all"]
```

:::note
Java annotation processors only see Java sources. For processors that read Kotlin code, use [KSP](#kotlin-symbol-processing-ksp). The [Lombok framework](../frameworks/#lombok) is the exception: it adds a Kotlin compiler plugin so Kotlin code can use what Lombok generates.
:::

## Kotlin Symbol Processing (KSP)

[KSP](https://kotlinlang.org/docs/ksp-overview.html) processors read your Kotlin (and Java) sources and generate code before they compile. Moshi codegen, Room, Dagger and Hilt, Koin annotations and many more ship one. Declare the processor with `scope = "ksp"` and the library it generates code for as a normal dependency:

```toml title="kargo.toml"
[dependencies]
"com.squareup.moshi:moshi" = "1.15.2"
"com.squareup.moshi:moshi-kotlin-codegen" = { version = "1.15.2", scope = "ksp" }
```

```kotlin title="src/Main.kt"
package com.example.app

import com.squareup.moshi.JsonClass
import com.squareup.moshi.Moshi

@JsonClass(generateAdapter = true)
data class Person(val name: String, val age: Int)

fun main() {
    val adapter = Moshi.Builder().build().adapter(Person::class.java) // uses the generated PersonJsonAdapter
    println(adapter.toJson(Person("Ada", 36)))
}
```

```console
$ kargo run
  Processing app v0.1.0 (jvm, ksp)
   Compiling app v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.57s
     Running `java com.example.app.MainKt`
{"name":"Ada","age":36}
```

On the JVM target, Kargo runs KSP2 standalone (`symbol-processing-aa-embeddable`) right before the Kotlin compiler, once for the main sources and once for the tests. The Kotlin and Java files it generates under `target/<profile>/jvm/generated/ksp/<main|test>/` are compiled with your sources, and generated resources are copied into the output. KSP reruns only when a source, a dependency or a processor changed. It analyzes the sources with your `[kotlin] language-version` and `api-version`, which default to the major and minor version of `[kotlin] version`.

Kargo uses KSP 2.3.12 unless you pick another version in `[kotlin]`. It applies to the JVM and to the Gradle KSP plugin on Android:

```toml title="kargo.toml"
[kotlin]
ksp = "2.3.10"
```

Since 2.3.0, KSP versions no longer follow Kotlin versions, so one KSP release works with several Kotlin versions.

As with `processor`, KSP processors and their dependencies never reach the compile or runtime classpath, are not packaged, and are pinned in `kargo.lock` (under `jvm/ksp`, and `jvm/test-ksp` when you have test-only processors). Processors in `[dependencies]` or `[target.jvm.dependencies]` run for the main and test sources; a processor in `[dev-dependencies]` runs for the tests only. On Android, the scope maps to the Gradle KSP plugin's `ksp(...)` and `kspTest(...)` configurations.

KSP processes JVM and Android code. Wasm, JS and Native ignore `ksp` dependencies.

:::note[What about kapt?]
Kargo doesn't support kapt. It is in maintenance mode, slow (it generates Java stubs for all your Kotlin code first) and the libraries that needed it now ship KSP processors. A Java-only processor that runs on Java sources works with [`scope = "processor"`](#annotation-processors).
:::

## BOMs (platforms)

A BOM ("bill of materials") manages the versions of a family of libraries so you don't have to. Import one with `bom = true`, then leave the version out of the libraries it manages:

```toml title="kargo.toml"
[dependencies]
"software.amazon.awssdk:bom" = { version = "2.31.0", bom = true }
"software.amazon.awssdk:s3" = {}
"software.amazon.awssdk:dynamodb" = {}
```

Or from the command line:

```sh
kargo add software.amazon.awssdk:bom:2.31.0 --bom
```

## Version conflicts

When two libraries depend on different versions of the same module, Kargo does what Gradle does: **the highest version wins**. Set `force = true` on a direct dependency to override that.

See what was picked with `kargo tree`. Conflicts are shown as `requested -> selected`, and subtrees already printed are marked `(*)`:

```sh
kargo tree                       # runtime classpath of the default target
kargo tree -c compile            # compile classpath
kargo tree -c test               # test classpath
kargo tree -t wasm               # another target
kargo tree --depth 1             # direct dependencies only
kargo tree --duplicates          # only modules requested in more than one version
kargo tree --invert g:util       # what depends on g:util, up to kargo.toml
```

### Why is this version here?

`kargo why` answers the question `kargo tree` can't: why a module is on the classpath, and why at that version.

```console
$ kargo why g:util
g:util:2.0 (jvm, runtime)
  selected 2.0, the highest version requested
  requested:
    1.0 -> 2.0  by g:left:1.0 (highest wins)
    2.0         by g:right:1.0
  path:
    app v0.1.0 -> g:left:1.0 -> g:util:2.0
```

It lists every version that was asked for and by whom (your `kargo.toml`, another module, a BOM, a platform, a Gradle module metadata constraint, a relocation), says what decided between them (the highest wins, `force = true`, a version range, or a version `kargo.lock` keeps), and shows the shortest path from `kargo.toml` to the module (`--all-paths` for every one). Use `-t` and `-c` to pick the target and classpath, as with `kargo tree`, and `--format json` for scripts. To change the outcome, declare the module yourself at the version you want (the highest still wins), or with `force = true` to go lower. See [`kargo why`](../../reference/commands/#kargo-why).

### What does a dependency offer?

`kargo inspect` shows where a dependency's jar is and what is in it, so you don't have to find the jar in the cache and run `javap` on it yourself. This is how you see, for example, that a library's `JsonNode` declares its own `map` that shadows Kotlin's `Iterable.map`.

```console
$ kargo inspect org.jetbrains.kotlin:kotlin-stdlib kotlin.Pair
  Inspecting org.jetbrains.kotlin:kotlin-stdlib:2.4.21 (jvm, runtime) /home/me/.kargo/cache/maven/org/jetbrains/kotlin/kotlin-stdlib/2.4.21/kotlin-stdlib-2.4.21.jar
Compiled from "Tuples.kt"
public final class kotlin.Pair<A, B> implements java.io.Serializable {
  public kotlin.Pair(A, B);
  public final A getFirst();
  public final B getSecond();
  ...
}
```

Without a class it lists the public classes of the jar, one per line. A class is named in full (`com.acme.Outer.Inner` or `com/acme/Outer$Inner`) or by the end of its name (`Inner`, `Outer.Inner`) when only one class of the jar matches. The module is looked up in the classpath a build uses, so the jar is the version in `kargo.lock`. `-c` picks the classpath, `--visibility` the members printed. The output is the JVM view of a class, so a Kotlin property shows as its getter and setter. See [`kargo inspect`](../../reference/commands/#kargo-inspect). The cache the jar lives in is described in [the lockfile reference](../../reference/lockfile/), and `kargo classpath` prints the paths of every jar.

## Dependencies of one profile

A dependency that only some builds need goes in the profile table: an HTTP logger in debug builds, or a mock server in a staging build. It is added to the build of every target with that profile, and to no other profile:

```toml title="kargo.toml"
[profile.dev.dependencies]
"com.squareup.okhttp3:logging-interceptor" = "4.12.0"

[profile.staging]
inherits = "release"

[profile.staging.dependencies]
"com.squareup.okhttp3:mockwebserver" = "4.12.0"
```

`kargo build --profile staging` and `kargo run --profile staging` have the mock server, and `kargo build` does not. A profile with dependencies of its own resolves them in its own entry of `kargo.lock` (`jvm/runtime@staging`), and the others keep their shared entries. A dependency with the same artifact as one of the manifest replaces it for that profile. See [Profiles](../../reference/manifest/#profilename) for the keys, and [On Android](../android/) for how the Android target applies them.

## Multiplatform libraries

Kotlin Multiplatform libraries publish one Maven module per platform plus Gradle module metadata. Add the **common** coordinates and Kargo picks the right variant for each target:

```toml
[dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"   # -> .jar on JVM, .klib on Wasm/Native
```

Libraries that only exist for one platform belong in that target's section:

```toml
[target.jvm.dependencies]
"com.zaxxer:HikariCP" = "6.2.1"
```

## Local packages

Another Kargo package on disk can be a dependency too, built from source with your project:

```sh
kargo add --path ../mathlib
```

```toml title="kargo.toml"
[dependencies]
mathlib = { path = "../mathlib" }
```

Its jar (or klib, for Wasm, JS and Native) ends up on your classpaths, with its own Maven dependencies: the ones it marks `exported = true` to compile against, and the others at runtime only, like Gradle's `api` and `implementation`. To build several packages together with one `kargo.lock`, make them members of a workspace. See [Workspaces and path dependencies](../workspaces/).

## Repositories

Maven Central is used first, and Google's Maven repository is added automatically for Android and Compose. Add others in `[repositories]`; they are asked after those, in the order you write them:

```toml title="kargo.toml"
[repositories]
jitpack = "https://jitpack.io"

# Private repository with credentials from environment variables
company = { url = "https://repo.example.com/maven", username-env = "REPO_USER", password-env = "REPO_PASS" }

# A local directory
libs = "file:///home/me/libs"

# The local Maven repository (~/.m2/repository)
local = "maven-local"
```

Credentials are never written in `kargo.toml`; Kargo reads them from the named environment variables. GitHub Packages, GitLab and AWS CodeArtifact repositories need no names: Kargo reads `GITHUB_TOKEN`, `CI_JOB_TOKEN` or `CODEARTIFACT_AUTH_TOKEN` (see [Hosted repositories](../../reference/manifest/#hosted-repositories)).

To send every repository through a company mirror, or build from a copy of the dependencies kept in the project, see [Offline and corporate builds](../offline-and-mirrors/).

`maven-local` holds what `kargo publish --local`, `mvn install` and Gradle's `publishToMavenLocal` install, so a library you publish locally can be used by your other projects right away. See [Publishing a library](../publishing/).

Credentials go over `https://`. A repository with credentials on a plain `http://` URL (other than `localhost`) gets a warning, and `--locked` refuses to send them.

### Choosing which repository serves a library

By default every repository is asked for every library, in order, and the version lists of all of them are merged. For a private library that is a risk: its coordinates are first looked up on Maven Central, and if someone publishes a higher version of `com.corp:internal-lib` on a public repository, version ranges, `latest` and `kargo add` pick it (*dependency confusion*). Tell Kargo which repository owns which groups:

```toml title="kargo.toml"
[repositories.corp]
url = "https://nexus.corp/maven"
username-env = "NEXUS_USER"
password-env = "NEXUS_PASSWORD"
include-groups = ["com.corp.*"]

[repositories.maven-central]
exclude-groups = ["com.corp.*"]
```

- **`include-groups`** makes the repository the only one asked for those groups, and asks it for nothing else. Versions of `com.corp` libraries then come from `corp` alone, whatever a public repository publishes.
- **`exclude-groups`** keeps a repository from being asked for those groups. With `include-groups` on `corp`, the `exclude-groups` on `maven-central` above changes nothing; it is useful on its own to keep a group away from one repository.
- `"com.corp"` matches that group only; `"com.corp.*"` matches `com.corp` and every group below it, such as `com.corp.tools`.
- `maven-central` and `google` without a `url` configure the built-in repositories. They keep their place at the front.

The same rules go into the Gradle build Kargo generates for [Android](../android/) (`exclusiveContent` and `content` blocks), so they hold there too.

To leave out Maven Central and Google Maven altogether, for example when a company mirror serves everything, set `defaults = false`:

```toml title="kargo.toml"
[repositories]
defaults = false
corp = "https://artifactory.corp/maven-virtual"
```

Only the repositories you list are used then, also for Compose and Android, so the mirror must have the libraries they need. A built-in one can be listed again by name, `google = {}`. To redirect repositories for every project on a machine instead, use [`[mirrors]`](../offline-and-mirrors/#repository-mirrors). Android's Gradle build still gets its own plugins from Google, Maven Central and the Gradle Plugin Portal, or their mirrors.

## SNAPSHOT versions

A version that ends in `-SNAPSHOT` is a library's work in progress, redeployed over and over. Declare it like any other version and add the repository that holds it:

```toml title="kargo.toml"
[dependencies]
"com.acme:lib" = "1.0-SNAPSHOT"

[repositories]
snapshots = "https://repo.example.com/snapshots"
```

Repositories such as Nexus, Artifactory and GitHub Packages (and `mvn deploy`) don't store a file called `lib-1.0-SNAPSHOT.jar`. Each deployment is a **timestamped build**, `lib-1.0-20240101.120000-1.jar`, and `1.0-SNAPSHOT/maven-metadata.xml` says which build is the newest of each file (the jar, the POM, the Gradle module metadata, and each classifier). Kargo reads that file and downloads the newest build. A repository without that metadata, or one that keeps the plain `lib-1.0-SNAPSHOT.jar` name (what `mvn install` writes into `~/.m2/repository`), is used as it is.

Builds don't move under you:

- **`kargo.lock` records the build.** The locked path is the timestamped file, so every build and every machine uses the same one until you decide otherwise. `kargo build --locked` in CI stays reproducible.
- **`kargo update` moves to the newest build**, and prints which one:

  ```console
  $ kargo update
      Updating com.acme:lib v1.0-SNAPSHOT (20240101.120000-1) -> v1.0-SNAPSHOT (20240102.080000-2)
       Updated kargo.lock
  ```

  `kargo update com.acme:other` leaves the SNAPSHOTs it wasn't asked about on their locked builds. `kargo build --refresh` ignores the lockfile for one run and picks the newest build as well.
- **Metadata is re-checked daily.** Without a lockfile entry (a new dependency, say), Kargo reuses a remote repository's SNAPSHOT metadata for up to 24 hours. `file:` repositories are read every time, so a snapshot you just published to a local directory is seen immediately. A SNAPSHOT stored under its plain name is compared with the repository's checksum on the same schedule, and downloaded again if it changed.

:::caution
Repositories often delete old snapshot builds. If a locked build is gone, the build fails with `could not find ...`; run `kargo update` to lock the current one.
:::

## The lockfile

`kargo.lock` records the exact version of every resolved module and the SHA-256 of every file it downloads. Kargo checks each file it uses against those checksums, cached copies included. Commit it, so everyone builds with the same dependencies.

```sh
kargo update        # re-resolve everything and rewrite kargo.lock
kargo update g:a    # re-resolve one module; everything else keeps its locked version
kargo update g:a --precise 1.2.3   # lock one module, even a transitive one, at an exact version
kargo update --dry-run  # show what would change without writing kargo.lock
kargo fetch         # download everything now (useful before going offline)
kargo build --locked    # fail if kargo.lock would change (use this in CI)
kargo build --offline   # never touch the network
kargo build --refresh   # ignore cached metadata and re-check repositories
```

See the [kargo.lock reference](../../reference/lockfile/) for the format.

## When the network fails

Kargo retries a request up to five times when the server answers HTTP 408, 429, 500, 502, 503 or 504, or the connection times out or drops. DNS, TLS and invalid URL errors fail at once. After such a failure Kargo remembers the host for the rest of the run and every further request to it fails immediately with "cannot reach <host>; check the network connection and the repository URL, or use `--offline`", instead of retrying for each module.

When a file is missing and a repository cannot be reached, the error lists every repository with what happened to it, so a typo in a coordinate is not mistaken for a network problem:

```console
error: failed to resolve com.example:nothere:1.0
Caused by:
  `com/example/nothere/1.0/nothere-1.0.pom` was not found everywhere it could be asked for:
    maven-central (repo.maven.apache.org): not found
    bad (nonexistent-host.invalid): could not be reached
```

The repositories are asked in order and Kargo stops at the unreachable one, since the file may be there: the ones after it show as `not asked`, and Kargo never takes from a later repository what an earlier one that is down may hold.

A failed request for a module's `maven-metadata.xml` keeps the cached copy, so `kargo add`, `kargo outdated` and `kargo upgrade` still work from it; Kargo prints `using cached metadata from <age> ago`. The cached copy is deleted only when the repository says the file does not exist. A `403` for a `.sha256`, `.sha1`, `.module` or `maven-metadata.xml` file counts as absent, because repositories on object storage answer 403 for missing keys.
