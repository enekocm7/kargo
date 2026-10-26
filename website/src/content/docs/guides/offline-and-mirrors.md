---
title: Offline and corporate builds
description: Vendor every dependency into the repository with kargo vendor, and send downloads through mirrors, a proxy and your own certificate authorities.
---

Kargo downloads dependencies from Maven Central, Google's Maven repository and your `[repositories]`, and toolchains (JDKs, GraalVM, Gradle, language servers) from their public hosts. Two setups need something else:

- **Air-gapped builds and reproducible archives**: copy every dependency into the project with [`kargo vendor`](#vendoring-dependencies) and build from that copy alone.
- **Corporate networks** that block the public hosts: send every download through an Artifactory or Nexus [mirror](#repository-mirrors), a [proxy](#proxies-and-certificates) and your company's certificate authority.

Both are configured in Kargo's own configuration file rather than in `kargo.toml`, since they describe the network a build runs in, not the project.

## Configuration files

Kargo reads two files, in this order, and the second wins key by key:

| File | Applies to |
|---|---|
| `~/.kargo/config.toml` (`$KARGO_HOME/config.toml`) | Every project you build |
| `.kargo/config.toml` in the project, or in the nearest directory above the current one that has one | That project |

Both are optional, and unknown keys are errors. `kargo info` lists the files it read on its `config:` line, and shows where each repository is fetched from:

```console
$ kargo info
...
repositories: maven-central (via https://artifactory.corp/maven-virtual), jitpack (via https://artifactory.corp/maven-virtual)
config:       /home/me/.kargo/config.toml
```

`kargo info --format json` adds an `effective-url` to every repository.

## Vendoring dependencies

`kargo vendor` copies every Maven file a build of the project needs into `vendor/`, laid out as a Maven repository:

```console
$ kargo vendor
    Vendored 214 files into /home/me/app/vendor (214 copied)
note: to build from it, pass `--vendored`, or add this to .kargo/config.toml:

[source]
vendored = true
```

`vendor/` holds every artifact of `kargo.lock`, checked against the SHA-256 it records, along with the POMs, Gradle module metadata (`.module`) and `maven-metadata.xml` files the resolver reads, and a `.sha1` checksum next to each file. What it holds is what [`kargo fetch`](../../reference/commands/#kargo-fetch) downloads for the enabled targets: the dependencies of every configuration, the compiler plugins, the JUnit console launcher, KSP and annotation processors, and the libraries of [`kargo bench`](../benchmarking/). `kargo.lock` is brought up to date first, as `kargo fetch` does; `--locked` makes `kargo vendor` fail instead.

Then build from it alone:

```sh
kargo build --vendored
```

or turn it on for the project, so every command uses it:

```toml title=".kargo/config.toml"
[source]
vendored = true
```

A vendored build resolves only from `vendor/`: Maven Central, Google and your `[repositories]` are never asked, and a file that is neither in `vendor/` nor in the cache (`~/.kargo/cache`) is an error. The lock stays valid, since `kargo.lock` fingerprints the repositories that `kargo.toml` declares, not the ones a build happens to use. Because `vendor/` also has the POMs and the version lists, the dependencies can be resolved again from it (after a `kargo.toml` change or with `kargo update --vendored`), as long as the versions they need are there.

| Option | Meaning |
|---|---|
| `[path]` | The directory to write, relative to the project root (default `[source] directory`, else `vendor`) |
| `--toolchains` | Also copy the Kotlin compiler and the Kotlin/Native distribution for this host, so a build on a machine with an empty `~/.kargo` works |
| `--sync` | Remove the files of the directory that the project no longer needs. Without it, `kargo vendor` only adds and updates files |

`--vendored` also reads `KARGO_VENDORED=1`. `kargo vendor` itself always uses the real repositories, even with `[source] vendored = true`.

### Building on an air-gapped machine

On a machine with network access:

```sh
kargo vendor --toolchains
git add vendor .kargo/config.toml
```

On the machine without it, with a JDK installed:

```sh
kargo build --offline
```

`--offline` keeps everything else (JDK and Gradle downloads, npm) from trying the network, and `[source] vendored = true` in the committed `.kargo/config.toml` makes the build use `vendor/`. To check that `vendor/` is complete before you ship it, build with an empty Kargo home: `KARGO_HOME=$(mktemp -d) kargo build --vendored --offline`.

:::note
Some downloads are not Maven files and are not vendored: a JDK that `[toolchain] jdk` pins (install one on the machine, or copy `~/.kargo/toolchains/jdk`), the [npm packages](../wasm/#npm-packages) of the Wasm target, the LLVM and sysroots that Kotlin/Native downloads on first use (run `kargo toolchain install` in the project, or copy `~/.kargo/toolchains/konan`), and the dependencies of the Android target, which Gradle resolves itself. `kargo vendor` warns about an Android target and skips it.
:::

| Key | Meaning |
|---|---|
| `[source] vendored` | `true` to resolve only from the vendored repository, as `--vendored` does |
| `[source] directory` | The vendored repository, relative to the project root (default `vendor`). `kargo vendor` writes it and `--vendored` reads it |

## Repository mirrors

`[mirrors]` replaces repositories with a mirror, for example an Artifactory or Nexus proxy of Maven Central:

```toml title="~/.kargo/config.toml"
[mirrors]
"*" = "https://artifactory.corp/maven-virtual"          # replaces every remote repository
"https://repo.maven.apache.org/maven2" = "https://nexus.corp/central"
"google" = "https://nexus.corp/google"

[mirrors.credentials."https://artifactory.corp"]
username-env = "ART_USER"
password-env = "ART_TOKEN"
```

A key names what the mirror replaces: a repository URL, a repository name (`maven-central`, `google`, or a name from `[repositories]`), or `*` for every remote repository that no other key names. Repositories on disk (`file:` URLs and `maven-local`) are never mirrored. Repositories that end up at the same mirror are asked once, for the groups any of them serves (see [`include-groups` and `exclude-groups`](../dependencies/#choosing-which-repository-serves-a-library)). With `"*"`, no request goes to Maven Central or Google; `kargo -v` prints the URL of every download, so you can check.

`[mirrors.credentials."<prefix>"]` gives the credentials for every URL that starts with the prefix (whole path segments; the longest prefix wins), read from the environment variables it names. They apply to mirrors, to repositories of `kargo.toml` that have no `username-env` of their own, and to [toolchain mirrors](#toolchain-mirrors). Unset variables send no credentials, so the server answers with HTTP 401.

Mirrors apply to every Maven download: dependencies, the Kotlin compiler, Kotlin/Native (which comes from Maven Central), compiler plugins and tools such as ktlint. The `settings.gradle.kts` that Kargo generates for the [Android target](../android/) uses them too: `google()`, `mavenCentral()`, `gradlePluginPortal()` and the extra repositories become `maven { url = uri(...) }` blocks of their mirrors, with a `credentials` block that reads the same environment variables.

## Toolchain mirrors

Toolchains are not Maven artifacts, so they have their own table, which replaces URL prefixes:

```toml title="~/.kargo/config.toml"
[toolchain-mirrors]
"https://api.adoptium.net" = "https://artifactory.corp/adoptium-api"
"https://github.com" = "https://artifactory.corp/github"
"https://services.gradle.org/distributions" = "https://artifactory.corp/gradle"
```

Every toolchain download goes through it, the longest matching prefix wins, and prefixes match whole path segments. These are the hosts Kargo downloads toolchains from:

| Toolchain | URLs |
|---|---|
| Temurin JDKs (`[toolchain] jdk`, `kargo toolchain install`) | `https://api.adoptium.net/v3` (the release lookup), then the archive it names, on `https://github.com/adoptium/...` |
| GraalVM (`[toolchain] graalvm`) | `https://api.foojay.io/disco/v3.0`, then the archive it names, usually on `https://github.com/...` |
| GraalVM reachability metadata | `https://github.com/oracle/graalvm-reachability-metadata/releases/...` |
| Gradle (Android builds) | `https://services.gradle.org/distributions` |
| Kotlin language servers and debug adapter | `https://download.jetbrains.com/language-server/...`, `https://github.com/fwcd/...` |

The Kotlin compiler and Kotlin/Native come from Maven repositories, so `[mirrors]` covers them.

## Proxies and certificates

Kargo honors the standard proxy variables: `HTTPS_PROXY`, `HTTP_PROXY` and `ALL_PROXY` (and their lowercase forms) name the proxy, and `NO_PROXY` lists the hosts that bypass it. `[http]` overrides them and adds your company's certificate authorities:

```toml title="~/.kargo/config.toml"
[http]
proxy = "http://proxy.corp:3128"
cainfo = "/etc/ssl/certs/corp-ca.pem"
```

| Key | Meaning |
|---|---|
| `proxy` | The proxy for every request (an `http://` or `https://` URL, optionally with `user:password@`). `""` turns off the proxy of the environment |
| `cainfo` | A PEM bundle of the certificate authorities to trust, in place of the built-in Mozilla roots, for a proxy that inspects TLS. A relative path is relative to the project for `.kargo/config.toml` and to the Kargo home for `~/.kargo/config.toml` |

They apply to every download Kargo makes itself. The Gradle of an Android build reads its own proxy settings (`systemProp.https.proxyHost` and friends in `~/.gradle/gradle.properties`).

## Every key

```toml title="~/.kargo/config.toml or .kargo/config.toml"
[mirrors]
"*" = "https://artifactory.corp/maven-virtual"

[mirrors.credentials."https://artifactory.corp"]
username-env = "ART_USER"
password-env = "ART_TOKEN"

[toolchain-mirrors]
"https://services.gradle.org/distributions" = "https://artifactory.corp/gradle"

[http]
proxy = "http://proxy.corp:3128"
cainfo = "corp-ca.pem"

[source]
vendored = true
directory = "vendor"

[toolchain]
auto-install = false

[build]
jobs = 4
```

`[build] jobs` is not about the network: it sets how many members and targets build at the same time on this machine (a number, or `"auto"`), below `-j`, `KARGO_JOBS` and the project's `[build] jobs`. See [Limiting parallelism](../build-performance/#limiting-parallelism).
