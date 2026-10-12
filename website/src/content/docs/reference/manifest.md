---
title: kargo.toml
description: The complete reference for kargo.toml.
---

`kargo.toml` is the only configuration file. Kargo looks for it in the current directory and its parents (or use `--manifest-path`). Unknown keys are errors, so typos are caught early.

Only `[package]` with `name` and `version` is required. A workspace's `kargo.toml` has a [`[workspace]`](#workspace) table instead of `[package]`.

## `[package]`

```toml
[package]
name = "my-app"                # required
version = "0.1.0"              # required
group = "com.example"          # Maven group; also the base Kotlin package in templates
description = "Does things"
authors = ["Ada <ada@example.com>"]
license = "MIT"
```

## `[kotlin]`

```toml
[kotlin]
version = "2.4.21"                        # default 2.4.21
language-version = "2.4"
api-version = "2.4"
ksp = "2.3.12"                            # default 2.3.12
opt-in = ["kotlin.ExperimentalStdlibApi"]
compiler-args = ["-Xcontext-parameters"]
warnings-as-errors = false
```

| Key | Default | Meaning |
|---|---|---|
| `version` | `2.4.21` | Kotlin compiler and standard library version |
| `language-version` | | `-language-version` |
| `api-version` | | `-api-version` |
| `ksp` | `2.3.12` | [KSP](../../guides/dependencies/#kotlin-symbol-processing-ksp) version for `scope = "ksp"` dependencies (the standalone tool on the JVM, the Gradle plugin on Android) |
| `opt-in` | `[]` | Annotations to opt in to everywhere |
| `compiler-args` | `[]` | Extra arguments for every compilation |
| `warnings-as-errors` | `false` | Fail on warnings |

## `[toolchain]`

```toml
[toolchain]
jdk = "21"     # "system" (default), a major version, or an exact release like "21.0.5+11"
```

See [Managing JDKs](../../guides/toolchains/).

## `[doc]`

Options for `kargo doc`. Every key is optional.

```toml
[doc]
includes = ["docs/module.md"]
source-link = "https://github.com/me/mylib/blob/main"
external-links = ["https://kotlinlang.org/api/kotlinx.coroutines/"]
dokka = "2.2.0"
```

| Key | Default | Meaning |
|---|---|---|
| `includes` | `[]` | Markdown files with module and package documentation, relative to the project root |
| `source-link` | none | `http(s)://` URL of the project root in a code browser; declarations link to their source lines |
| `external-links` | `[]` | `http(s)://` roots of Dokka or Javadoc sites (with a `package-list`) to link dependency types to |
| `dokka` | `2.2.0` | Exact Dokka version |

See [API documentation](../../guides/documentation/).

## `[fmt]`

Options for `kargo fmt`. Every key is optional. ktlint's style rules go in `.editorconfig`.

```toml
[fmt]
exclude = ["src/generated/**"]
ktlint = "1.8.0"
```

| Key | Default | Meaning |
|---|---|---|
| `exclude` | `[]` | Globs, relative to the project root, of files ktlint skips |
| `ktlint` | `1.8.0` | Exact ktlint version |

## `[lint]`

Options for `kargo lint`. Every key is optional.

```toml
[lint]
config = ["config/detekt.yml"]
baseline = "config/detekt-baseline.xml"
detekt = "1.23.8"
```

| Key | Default | Meaning |
|---|---|---|
| `config` | `["detekt.yml"]` if that file exists | detekt configuration files, relative to the project root, applied on top of detekt's defaults. Later files override earlier ones; each must exist |
| `baseline` | `detekt-baseline.xml` | Baseline file, relative to the project root. The default is used only if the file exists; a file you set must exist |
| `detekt` | `1.23.8` | Exact detekt version |

See [Formatting and linting](../../guides/formatting-and-linting/).

## `[test]`

Options for `kargo test`. Every key is optional.

```toml
[test]
fail-fast = false
```

| Key | Default | Meaning |
|---|---|---|
| `fail-fast` | `true` | Stop at the first target whose tests fail or don't compile. `false` tests every target and prints a result per target, like `kargo test --no-fail-fast` |

See [Testing every target](../../guides/testing/#testing-every-target).

## Targets

With no `[target.*]` section, the project targets the JVM with default settings. Otherwise exactly the listed targets are built.

### `[target.jvm]`

```toml
[target.jvm]
jvm-target = "25"                       # default "25", lowered to your JDK if it is older
main-class = "com.example.app.MainKt"   # default: detected from a Kotlin `fun main`; set it for a Java main
jvm-args = ["-Xmx1g"]                   # for kargo run and kargo test
javac-args = ["-Xlint:all"]             # extra javac options for Java sources
```

`javac-args` are appended to the `javac` command line for main and test Java sources, after the options Kargo sets (`-d`, `-encoding UTF-8`, `--release` from `jvm-target`, `-classpath`, and the [annotation processor](../../guides/dependencies/#annotation-processors) options). Use it for lint flags, `-parameters` or processor options such as `-Akey=value`. It has no effect on Kotlin sources; use `[kotlin] compiler-args` for those.

When you don't set `jvm-target`, Kargo targets Java 25, or the version of the JDK that builds the project if that JDK is older (a pinned `[toolchain] jdk`, else `JAVA_HOME` or `java` on `PATH`). The compiled classes then always run on the JDK you build with. Set `jvm-target` explicitly to support older Java runtimes than the one you build with.

Without `main-class`, Kargo scans the Kotlin sources for exactly one `main`: a top-level `fun main` (class `<package>.<File>Kt`) or a `@JvmStatic fun main` in an `object` (class `<package>.<Object>`) or a `companion object` (the enclosing class). If it finds none or several, `kargo run` says so and lists the candidates; set `main-class` to pick one.

### `[target.wasm]`

```toml
[target.wasm]
environment = "js"      # "js" (default) or "wasi"
package-manager = "pnpm" # "npm", "pnpm" or "bun"; default: the first one found on PATH
```

| Key | Default | Meaning |
|---|---|---|
| `environment` | `"js"` | `"js"` for Node.js and browsers, `"wasi"` for WASI runtimes |
| `package-manager` | first of `npm`, `pnpm`, `bun` on `PATH` | The tool that installs the [npm packages](../../guides/wasm/#npm-packages) your Wasm libraries need |

### `[target.native]`

```toml
[target.native]
platforms = ["host"]               # default ["host"]
kind = "executable"                # "executable" (default), "library", "shared" or "static"
entry-point = "com.example.main"
linker-options = ["-lcurl"]

[target.native.cinterop.sqlite]    # a C library bound with cinterop; one table per library
def = "interop/sqlite.def"         # default: interop/<name>.def
package = "sqlite3"
headers = ["sqlite3.h"]
compiler-options = ["-I/usr/include"]
linker-options = ["-lsqlite3"]
```

Valid platforms: `host`, `linux_x64`, `linux_arm64`, `mingw_x64`, `macos_x64`, `macos_arm64`, `ios_arm64`, `ios_x64`, `ios_simulator_arm64`, `watchos_arm64`, `watchos_simulator_arm64`, `tvos_arm64`, `tvos_simulator_arm64`, `android_arm64`, `android_arm32`, `android_x64`. Gradle-style names like `linuxX64` are accepted too.

| `[target.native.cinterop.<name>]` key | Default | Meaning |
|---|---|---|
| `def` | `interop/<name>.def` | The `.def` file, relative to the project root. It must exist unless only `headers` are given |
| `package` | the `.def` file's `package` | Kotlin package of the generated bindings |
| `headers` | `[]` | Extra headers to bind |
| `compiler-options` | `[]` | clang options for reading the headers (`-I`, `-D`, ...) |
| `linker-options` | `[]` | Linker options recorded in the bindings and used by every program that links them |

The name may contain letters, digits, `_` and `-`. See [Using C libraries](../../guides/native/#using-c-libraries).

### `[target.android]`

```toml
[target.android]
namespace = "com.example.app"    # required
application-id = "com.example.app"
kind = "application"             # or "library"
min-sdk = 24
target-sdk = 36
compile-sdk = 37
version-code = 1
version-name = "0.1.0"
agp-version = "..."              # Android Gradle Plugin version
```

| Key | Default |
|---|---|
| `application-id` | `namespace` |
| `kind` | `application` |
| `min-sdk` | `24` |
| `target-sdk` | `36` |
| `compile-sdk` | `37` |
| `version-code` | `1` |

### Per-target dependencies

Every target table can have a `dependencies` sub-table:

```toml
[target.jvm.dependencies]
"com.zaxxer:HikariCP" = "6.2.1"
```

## `[dependencies]` and `[dev-dependencies]`

```toml
[dependencies]
"group:artifact" = "1.0.0"
short-name = { group = "g", artifact = "a", version = "1.0" }   # artifact defaults to the key
other = { coords = "g:a:1.0" }
full = { version = "1.0", scope = "runtime", classifier = "x", extension = "jar", exclude = ["g:a", "g2:*"], force = true, transitive = false }
platform = { coords = "g:bom", version = "1.0", bom = true }
mathlib = { path = "../mathlib" }                               # another Kargo package, built from source
shared = { workspace = true }                                   # inherited from [workspace.dependencies]
```

| Key | Default | Meaning |
|---|---|---|
| `version` | | Version or Maven range. Optional when a BOM manages it. Required for BOMs. |
| `group`, `artifact` | | Coordinates when the key isn't `group:artifact` |
| `coords` | | `group:artifact[:version]` in one string |
| `scope` | `compile` | `compile`, `runtime`, `provided`, `processor` (a Java [annotation processor](../../guides/dependencies/#annotation-processors)) or `ksp` (a [KSP processor](../../guides/dependencies/#kotlin-symbol-processing-ksp)); `processor` and `ksp` apply to the JVM and Android only |
| `classifier` | | Artifact classifier |
| `extension` (alias `type`) | `jar` | Artifact extension |
| `exclude` | `[]` | `group:artifact` or `group:*` to exclude transitively |
| `force` | `false` | Use this version even if a newer one is requested |
| `transitive` | `true` | Include this dependency's dependencies |
| `bom` | `false` | Import as a BOM |
| `path` | | Directory of another Kargo package to build from source. The key must be its package name, and only `scope` can be set with it |
| `workspace` | | `true` to inherit the entry of the same name from `[workspace.dependencies]`; only `scope` can be set with it |

`[dev-dependencies]` are only used to compile and run tests. Path dependencies are described in [Workspaces and path dependencies](../../guides/workspaces/).

See [Managing dependencies](../../guides/dependencies/).

## `[repositories]`

```toml
[repositories]
jitpack = "https://jitpack.io"
company = { url = "https://repo.example.com/maven", username-env = "REPO_USER", password-env = "REPO_PASS" }
```

Maven Central is always used. Google Maven is added automatically for Android and Compose.

## `[frameworks]`

```toml
[frameworks]
serialization = true      # or { version, formats = ["json", "cbor", "protobuf", "properties", "hocon", "core"] }
coroutines = true         # or a version string: "1.11.0"
compose = true            # or { version, material3 = true }
ktor = { server = "netty", client = "cio", server-plugins = [], client-plugins = [], json = true, version = "3.6.0" }
spring-boot = true        # or { version, starters = ["web"], jpa = false }
lombok = true             # or { version, config = "lombok.config" }
```

Each can also be `false`. Default versions: serialization 1.11.0, coroutines 1.11.0, Compose 1.12.1 (Material 3 1.9.0), Ktor 3.6.0, Spring Boot 4.1.1, Lombok 1.18.48.

See [Built-in frameworks](../../guides/frameworks/).

## `[plugins.<name>]`

A compiler plugin:

```toml
[plugins.power-assert]
maven = "org.jetbrains.kotlin:kotlin-power-assert-compiler-plugin-embeddable:2.4.21"
id = "org.jetbrains.kotlin.powerassert"
options = { function = "kotlin.assert" }
```

A hook plugin:

```toml
[plugins.notify]
command = "./scripts/notify.sh"
args = ["--quiet"]
windows-command = "./scripts/notify.bat"   # optional: used instead of `command` on Windows
windows-args = ["--quiet"]                 # optional: used instead of `args` on Windows
hooks = ["post-build"]
env = { KEY = "value" }
```

Hooks: `pre-build`, `post-build`, `pre-test`, `post-test`, `pre-run`, `post-package`. On Windows a `command` ending in `.sh` runs through `sh`.

## `[tasks]`

```toml
[tasks]
simple = "echo hi"
full = { run = ["cmd1", "cmd2"], description = "...", depends-on = ["simple"], env = { K = "v" }, cwd = "sub", hooks = ["pre-build"] }
posix = { run = "echo $KARGO_PKG_NAME", shell = "sh" }
portable = { run = "echo $KARGO_PKG_NAME", windows-run = "echo %KARGO_PKG_NAME%" }
```

`shell` picks the shell the commands run in: `sh`, `bash`, `cmd`, `powershell` or `pwsh`. The default is `sh` on Unix and `cmd` on Windows. `windows-run` (a command or a list) replaces `run` on Windows. When it is not set, `run` is used on every platform.

See [Tasks and plugins](../../guides/tasks-and-plugins/).

## `[workspace]`

A workspace's `kargo.toml` has only a `[workspace]` table, no `[package]`. Its members share one `kargo.lock`, one `target/` and one Kotlin version.

```toml
[workspace]
members = ["app", "libs/core"]       # required: member directories, relative to this file

[workspace.kotlin]
version = "2.4.21"                   # the Kotlin version of every member

[workspace.dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
core = { path = "libs/core" }        # paths are relative to the workspace root
```

| Key | Meaning |
|---|---|
| `members` | Directories of the member packages. Globs are not supported |
| `[workspace.kotlin] version` | Kotlin version for every member. A member's `[kotlin] version` must match it if set |
| `[workspace.dependencies]` | Dependency entries members inherit with `name = { workspace = true }` |

See [Workspaces and path dependencies](../../guides/workspaces/).

## `[profile.dev]` and `[profile.release]`

```toml
[profile.release]
debug = false
optimize = true
compiler-args = ["-Xno-call-assertions"]
```

| Key | `dev` default | `release` default | Meaning |
|---|---|---|---|
| `debug` | `true` | `false` | Include debug information |
| `optimize` | `false` | `true` | Optimize (mainly affects Native and Wasm) |
| `compiler-args` | `[]` | `[]` | Extra compiler arguments for this profile |
