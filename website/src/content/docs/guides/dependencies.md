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
```

Kargo checks that the artifact exists before editing `kargo.toml` (skip that with `--no-verify`), and it keeps your file's comments and formatting.

Without a version, Kargo picks the newest release. Some libraries publish variants next to their plain versions (`0.8.0-0.6.x-compat`, `33.0.0-jre`); Kargo prefers the plain version and prints a note naming the variant it skipped. Pass the full coordinates to pick a variant.

To remove one, name the table it is in the same way as for `kargo add`:

```sh
kargo remove com.squareup.okio:okio
kargo rm io.mockk:mockk --dev
kargo rm com.google.guava:guava -t jvm
```

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

`kargo upgrade` changes what you declared in `kargo.toml`. To move versions in `kargo.lock` only, for example a transitive dependency, use `kargo update` (see [The lockfile](#the-lockfile)).

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

# Classifier and extension (type)
"org.lwjgl:lwjgl" = { version = "3.3.6", classifier = "natives-linux" }

# Version ranges use Maven syntax
"com.example:ranged" = "[1.2,2.0)"
```

| Key | Meaning |
|---|---|
| `version` | A version or Maven range. Required unless a BOM manages it. |
| `group`, `artifact`, `coords` | Alternatives to writing `"group:artifact"` as the key. |
| `scope` | `compile` (default), `runtime`, `provided`, `processor` or `ksp`. |
| `exclude` | `group:artifact` (or `group:*`) entries to drop from this dependency's tree. |
| `transitive` | `false` to ignore this dependency's own dependencies. |
| `force` | `true` to pin this version even if something else asks for a newer one. |
| `classifier`, `extension` | Select a specific file, such as `natives-linux` or `aar`. |
| `bom` | `true` to import this POM as a BOM (see below). |

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

KSP processes JVM and Android code. Wasm and Native ignore `ksp` dependencies.

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
```

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

Its jar (or klib, for Wasm and Native) and its own Maven dependencies end up on your classpaths. To build several packages together with one `kargo.lock`, make them members of a workspace. See [Workspaces and path dependencies](../workspaces/).

## Repositories

Maven Central is always used. Google's Maven repository is added automatically for Android and Compose. Add others in `[repositories]`:

```toml title="kargo.toml"
[repositories]
jitpack = "https://jitpack.io"

# Private repository with credentials from environment variables
company = { url = "https://repo.example.com/maven", username-env = "REPO_USER", password-env = "REPO_PASS" }

# A local directory
local = "file:///home/me/.m2/repository"
```

Credentials are never written in `kargo.toml`; Kargo reads them from the named environment variables.

## The lockfile

`kargo.lock` records the exact version of every resolved module and the SHA-256 of every downloaded file. Commit it, so everyone builds with the same dependencies.

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
