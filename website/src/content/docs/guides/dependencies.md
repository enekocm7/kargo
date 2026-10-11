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
kargo add com.google.guava:guava -t jvm          # only for the JVM target
kargo add org.jetbrains.kotlinx:kotlinx-io-core --pre   # allow a pre-release as "latest"
```

Kargo checks that the artifact exists before editing `kargo.toml` (skip that with `--no-verify`), and it keeps your file's comments and formatting.

To remove one:

```sh
kargo remove com.squareup.okio:okio
kargo rm io.mockk:mockk --dev
```

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
| `scope` | `compile` (default), `runtime` or `provided`. |
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
kargo fetch         # download everything now (useful before going offline)
kargo build --locked    # fail if kargo.lock would change (use this in CI)
kargo build --offline   # never touch the network
kargo build --refresh   # ignore cached metadata and re-check repositories
```

See the [kargo.lock reference](../../reference/lockfile/) for the format.
