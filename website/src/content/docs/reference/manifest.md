---
title: kargo.toml
description: The complete reference for kargo.toml.
---

`kargo.toml` is the only configuration file. Kargo looks for it in the current directory and its parents (or use `--manifest-path`). Unknown keys are errors, so typos are caught early.

Only `[package]` with `name` and `version` is required. A workspace's `kargo.toml` has a [`[workspace]`](#workspace) table instead of `[package]`.

:::tip
Your editor can complete and check these keys as you type with Kargo's JSON Schema for `kargo.toml`. See [Editor integration](../../guides/editors/#completion-and-validation-in-kargotoml).
:::

## `[package]`

```toml
[package]
name = "my-app"                # required
version = "0.1.0"              # required
group = "com.example"          # Maven group; also the base Kotlin package in templates
description = "Does things"
authors = ["Ada <ada@example.com>"]
license = "MIT"
homepage = "https://example.com/my-app"
repository = "https://github.com/me/my-app"
publish = true
```

In a workspace, a member can inherit `version`, `group`, `description`, `authors`, `license`, `homepage` and `repository` from [`[workspace.package]`](#workspace) with `version.workspace = true`.

`version` names the jar and is the version in its Maven coordinates. Any string is accepted, but Kargo [warns](../warnings/#odd-package-versions) about one that doesn't start with a digit or that uses anything besides letters, digits and `.`, `-`, `_`, `+`.

The other keys describe the package when it is [published](../../guides/publishing/) and fill in its POM:

| Key | POM | Meaning |
|---|---|---|
| `group` | `<groupId>` | Required to publish |
| `description` | `<description>` | Required by Maven Central |
| `authors` | `<developers>` | `Name <email>` entries; required by Maven Central |
| `license` | `<licenses>` | An SPDX expression such as `MIT` or `MIT OR Apache-2.0`; each license links to its page on spdx.org. Required by Maven Central |
| `homepage` | `<url>` | An http(s) URL; defaults to `repository` |
| `repository` | `<scm>` | The source repository, an http(s) URL. The SCM connection is `scm:git:<repository>`. Required by Maven Central |
| `publish` | | `false` makes `kargo publish` refuse the package (default `true`) |

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
| `warnings-as-errors` | `false` | Fail on warnings. The compiler's `warnings found and -Werror specified` summary is printed [after the diagnostics](../warnings/#warnings-as-errors) |

In a workspace, `[workspace.kotlin]` sets these for every member: a member's `opt-in` and `compiler-args` are added to the workspace's, and its other keys take precedence (see [`[workspace]`](#workspace)).

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
timeout = "10m"
test-timeout = "30s"
jvm-args = ["-Xmx1g"]
```

| Key | Default | Meaning |
|---|---|---|
| `fail-fast` | `true` | Stop at the first target whose tests fail or don't compile. `false` tests every target and prints a result per target, like `kargo test --no-fail-fast` |
| `timeout` | none | How long the whole test run may take, such as `"30s"`, `"5m"` or `"1h"`, counted from when the tests start running. The run is stopped and fails when it passes. Applies to the JVM, Native and Wasm targets, not Android |
| `test-timeout` | none | How long one JVM test may take, such as `"30s"`. A test that runs longer fails, and the run goes on. Applies to tests run by JUnit Jupiter |
| `jvm-args` | `[]` | JVM arguments for the test process, after `[target.jvm] jvm-args` |

See [Testing every target](../../guides/testing/#testing-every-target) and [Timeouts](../../guides/testing/#timeouts).

## `[publish]`

Options for `kargo publish`. Every key is optional.

```toml
[publish]
repository = "maven-central"
sign = true
sources = true
javadoc = "dokka"
central-publishing = "manual"
```

| Key | Default | Meaning |
|---|---|---|
| `repository` | none | Where `kargo publish` publishes without `--repository`: `maven-central`, `maven-local` or a [`[repositories]`](#repositories) entry |
| `sign` | `true` for Maven Central, else `false` | Sign every file with GnuPG (`.asc`). Maven Central requires it |
| `sources` | `true` | Publish `-sources.jar` files |
| `javadoc` | `true` (`"dokka"`) | The `-javadoc.jar`: `"dokka"` (or `true`) holds the HTML that [Dokka](../../guides/documentation/) generates, `"empty"` only a manifest (enough for Maven Central), `false` none |
| `central-publishing` | `"manual"` | What Maven Central does with a validated upload: `"manual"` waits for you to release it in the Central Portal, `"automatic"` releases it |

See [Publishing a library](../../guides/publishing/).

## Targets

With no `[target.*]` section, the project targets the JVM with default settings. Otherwise exactly the listed targets are built.

### `[target.jvm]`

```toml
[target.jvm]
jvm-target = "25"                       # 8 (or 1.8) and up; default "25", lowered to your JDK if it is older
main-class = "com.example.app.MainKt"   # default: detected from a Kotlin `fun main`; set it for a Java main
jvm-args = ["-Xmx1g"]                   # for kargo run and kargo test
javac-args = ["-Xlint:all"]             # extra javac options for Java sources
```

On JDK 21 and newer, `kargo run` and `kargo test` put `-XX:+EnableDynamicAgentLoading` and `--enable-native-access=ALL-UNNAMED` before `jvm-args`, so libraries that load an agent (MockK) or native code (Compose) don't print JVM warnings into your output. Turn one off by adding the opposite option, for example `jvm-args = ["-XX:-EnableDynamicAgentLoading"]`.

`javac-args` are appended to the `javac` command line for main and test Java sources, after the options Kargo sets (`-d`, `-encoding UTF-8`, `--release` from `jvm-target`, `-classpath`, and the [annotation processor](../../guides/dependencies/#annotation-processors) options). Use it for lint flags, `-parameters` or processor options such as `-Akey=value`. It has no effect on Kotlin sources; use `[kotlin] compiler-args` for those.

`jvm-target` is a Java version: `8`, or `9` and up (`11`, `17`, `21`, ...). `1.8` is accepted as another spelling of `8`. Kargo checks the value when it loads `kargo.toml`, so a typo such as `"abc"` or `"1.9"` fails with the list of valid values before any compiler starts. It passes `1.8` to the Kotlin compiler (which doesn't accept `8`) and `--release 8` to `javac`.

When you don't set `jvm-target`, Kargo targets Java 25, or the version of the JDK that builds the project if that JDK is older (a pinned `[toolchain] jdk`, else `JAVA_HOME` or `java` on `PATH`). The compiled classes then always run on the JDK you build with. Set `jvm-target` explicitly to support older Java runtimes than the one you build with.

An explicit `jvm-target` can't be newer than the JDK that builds the project, pinned or from `JAVA_HOME`/`PATH`, because that JDK can't compile for it. Kargo stops with an error that suggests setting `[toolchain] jdk` to a matching version:

```console
$ kargo build
error: [target.jvm] jvm-target 26 is newer than JDK 25, which builds the project; set `[toolchain] jdk = "26"` in kargo.toml to build with a newer JDK, or lower `jvm-target`
```

Without `main-class`, Kargo scans the Kotlin sources for exactly one `main`: a top-level `fun main` (class `<package>.<File>Kt`) or a `@JvmStatic fun main` in an `object` (class `<package>.<Object>`) or a `companion object` (the enclosing class). If it finds none or several, `kargo run` says so and lists the candidates; set `main-class` to pick one, run another with `kargo run --main <class>`, or declare each program as a [`[[bin]]`](#bin).

### `[target.wasm]`

```toml
[target.wasm]
environment = "js"        # "js" (default), "browser" or "wasi"
kind = "application"      # "application" (default) or "library"
module-name = "hello"     # name of the .mjs and .wasm files; default: the package name
npm-name = "@acme/hello"  # npm package name of a library; default: the lowercase package name
package-manager = "pnpm"  # "npm", "pnpm" or "bun"; default: the first one found on PATH
wasm-opt = false          # false, or the path of a wasm-opt executable; default: wasm-opt on PATH

[target.wasm.npm-dependencies]
"left-pad" = "1.3.0"      # npm packages besides those your Wasm libraries ask for
```

| Key | Default | Meaning |
|---|---|---|
| `environment` | `"js"` | `"js"` for Node.js, `"browser"` for web pages (`kargo run` serves them and `kargo test` uses a headless browser), `"wasi"` for WASI runtimes. See [WebAssembly](../../guides/wasm/#setup) |
| `kind` | `"application"` | `"application"` for a program with a `main`, `"library"` for an npm package of `@JsExport` functions with TypeScript declarations. A library needs `environment = "js"` or `"browser"` |
| `module-name` | the package name | Name of the compiled module's files (`<name>.mjs`, `<name>.wasm`). Letters, digits, `-`, `_` and `.` |
| `npm-name` | the lowercase package name | `name` in the `package.json` of a library. May be scoped (`@scope/name`); only valid with `kind = "library"` |
| `package-manager` | first of `npm`, `pnpm`, `bun` on `PATH` | The tool that installs the [npm packages](../../guides/wasm/#npm-packages) your project needs |
| `wasm-opt` | `true` | In release builds, run Binaryen's `wasm-opt` over the module when there is one on `PATH`. `false` never runs it; a string is the path of the executable to run, relative to the project. See [Release builds](../../guides/wasm/#release-builds) |
| `npm-dependencies` | none | A table of npm packages (`name = "version"`) to install in addition to those the klibs ask for. Your version wins on a conflict. Each gets [Kotlin declarations](../../guides/wasm/#kotlin-declarations-for-npm-packages) generated from its TypeScript ones |

With `optimize = true`, as in the [release profile](#profiledev-and-profilerelease), a Wasm build removes code nothing reaches, runs `wasm-opt` and reports the size of the output.

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
gradle-version = "..."           # Gradle version that runs the build
minify = true                    # R8 for release builds
proguard-files = ["android/proguard-rules.pro"]
build-config = { API_URL = "https://api.example.com", LOGGING = false }

[target.android.signing.release]
store-file = "release.jks"
store-password-env = "KS_PASS"
key-alias = "upload"
key-password-env = "KEY_PASS"

[target.android.flavors.free]
dimension = "tier"
application-id-suffix = ".free"
```

| Key | Default |
|---|---|
| `application-id` | `namespace` |
| `kind` | `application` |
| `min-sdk` | `24` |
| `target-sdk` | `36` |
| `compile-sdk` | `37` |
| `version-code` | `1` |
| `agp-version` | `9.4.1` |
| `gradle-version` | The minimum Gradle version `agp-version` requires (`9.6.0` for AGP 9.4.1). Must not be older than that. A `gradlew` in the project root is used instead when present. See [Android](../../guides/android/#gradle) |
| `minify` | `false`. Shrink, obfuscate and optimize release builds with R8 |
| `shrink-resources` | `minify`, for applications. Remove unused resources from release builds; needs `minify = true` |
| `proguard-files` | `[]`. R8 rule files for release builds, relative to the project root, after AGP's `proguard-android-optimize.txt` |
| `consumer-proguard-files` | `[]`. Libraries only: R8 rules shipped in the AAR for the apps that use it |
| `build-config` | `{}`. `BuildConfig` fields: a string becomes a `String`, a boolean a `boolean`, an integer an `int` (a `long` beyond 32 bits) |
| `flavor-dimensions` | The dimensions of `flavors`. Required, in priority order, when the flavors use more than one |

`[target.android.signing.<build-type>]` signs the `debug` or `release` build type (applications only). Passwords are never written to `kargo.toml` or the generated Gradle files: Gradle reads them from the environment variables you name when it runs.

| Key | Meaning |
|---|---|
| `store-file` | Required. The keystore, relative to the project root |
| `store-password-env` | Required. Environment variable holding the keystore password |
| `key-alias` | Required. The key in the keystore |
| `key-password-env` | Environment variable holding the key password. Default: `store-password-env` |
| `store-type` | `jks` or `pkcs12`. Default: detected |

`[target.android.flavors.<name>]` declares a product flavor. Flavor names use letters, digits and `_`.

| Key | Meaning |
|---|---|
| `dimension` | Required. The flavor dimension it belongs to |
| `application-id-suffix` | Appended to the application id (`.free`) |
| `version-name-suffix` | Appended to the version name |
| `version-code` | Overrides `version-code` |
| `min-sdk` | Overrides `min-sdk` |
| `build-config` | More `BuildConfig` fields for this flavor |

See [Release builds](../../guides/android/#release-builds) and [Product flavors](../../guides/android/#product-flavors).

### Per-target dependencies

Every target table can have a `dependencies` sub-table:

```toml
[target.jvm.dependencies]
"com.zaxxer:HikariCP" = "6.2.1"
```

## `[[bin]]`

Named programs besides the default one, run with `kargo run --bin <name>` and packaged with `kargo package --bin <name>`. Repeat the table for each program.

```toml
[[bin]]
name = "migrate"
path = "src/tools/Migrate.kt"            # the file whose `main` to run; works for every target

[[bin]]
name = "report"
main = "com.example.report.Cli"          # JVM main class
entry-point = "com.example.report.main"  # Kotlin/Native entry function
```

| Key | Meaning |
|---|---|
| `name` | Required. Letters, digits, `-` and `_`, starting with a letter; unique |
| `path` | A `.kt` file, relative to the project root, that declares exactly one `main` |
| `main` | The JVM main class |
| `entry-point` | The Kotlin/Native entry function |

Set `path`, or `main` and/or `entry-point`, but not both. The sources stay in `src/`. An executable Native target links one extra executable per bin with a native entry point, into `target/<profile>/native/<platform>/bin/`. See [Several programs in one project](../../guides/multiple-programs/).

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
api = { version = "1.0", exported = true }                      # part of this package's API

[dev-dependencies]
mathlib = { path = "../mathlib", test-fixtures = true }         # also its test fixtures
```

| Key | Default | Meaning |
|---|---|---|
| `version` | | Version or Maven range. Optional when a BOM manages it. Required for BOMs. |
| `group`, `artifact` | | Coordinates when the key isn't `group:artifact` |
| `coords` | | `group:artifact[:version]` in one string |
| `scope` | `compile` | `compile`, `runtime`, `provided`, `processor` (a Java [annotation processor](../../guides/dependencies/#annotation-processors)) or `ksp` (a [KSP processor](../../guides/dependencies/#kotlin-symbol-processing-ksp)); `processor` and `ksp` apply to the JVM and Android only |
| `classifier` | | Artifact classifier. Selects one file of the module; declare the module again without it for the main jar ([native libraries](../../guides/dependencies/#classifiers-and-native-libraries)) |
| `extension` (alias `type`) | `jar` | Artifact extension |
| `exclude` | `[]` | `group:artifact` or `group:*` to exclude transitively |
| `force` | `false` | Use this version even if a newer one is requested |
| `transitive` | `true` | Include this dependency's dependencies |
| `bom` | `false` | Import as a BOM |
| `path` | | Directory of another Kargo package to build from source. The key must be its package name, and only `scope`, `exported` and `test-fixtures` can be set with it |
| `exported` | `false` | Part of this package's API, like Gradle's `api`: packages that depend on this one through `path` compile against it too, and its POM scope is `compile`. Otherwise they only get it at runtime, like Gradle's `implementation` ([what comes along](../../guides/workspaces/#what-comes-along)) |
| `test-fixtures` | `false` | With `path`, in `[dev-dependencies]`: the tests also get the dependency's [test fixtures](../../guides/workspaces/#test-fixtures) |
| `workspace` | | `true` to inherit the entry of the same name from `[workspace.dependencies]`; only `scope`, `exported` and `test-fixtures` can be set with it |

A module can be declared several times with different `classifier` or `extension` values (the key must differ); declaring the same `group:artifact:classifier:extension` twice is an error. All of them share the module's version.

`[dev-dependencies]` are only used to compile and run tests. Path dependencies are described in [Workspaces and path dependencies](../../guides/workspaces/).

See [Managing dependencies](../../guides/dependencies/).

## `[repositories]`

```toml
[repositories]
jitpack = "https://jitpack.io"
company = { url = "https://repo.example.com/maven", username-env = "REPO_USER", password-env = "REPO_PASS" }
```

Maven Central is always used. Google Maven is added automatically for Android and Compose.

The value `"maven-local"` stands for the local Maven repository, `~/.m2/repository` (or `$KARGO_MAVEN_LOCAL`), where `kargo publish --local`, `mvn install` and Gradle's `publishToMavenLocal` put libraries:

```toml
[repositories]
local = "maven-local"
```

`kargo publish --repository <name>` uploads to a repository listed here, with the same credentials.

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

A task can have any name without spaces, but one named like a built-in command (`build`, `test`, `fmt`, ...) or its alias (`b`, `t`, ...) runs only as `kargo task <name>`, and Kargo [warns](../warnings/#tasks-hidden-by-built-in-commands) about it.

See [Tasks and plugins](../../guides/tasks-and-plugins/).

## `[env]`

```toml
[env]
APP_NAME = "demo"
LOG_LEVEL = { value = "debug", force = true, relative = false, required = false }
CONFIG_DIR = { value = "config", relative = true }
DATABASE_URL = { required = true }
```

The variables the program gets with `kargo run`, `kargo test`, tasks and hooks. A variable is a string, an integer, a boolean or a table with these keys:

| Key | Default | Meaning |
|---|---|---|
| `value` | none | The value. Required unless `required = true`, and not allowed with it |
| `force` | `false` | Use `value` even when the shell or a `.env` file sets the variable |
| `relative` | `false` | Resolve `value` against the package's directory. Needs a `value` |
| `required` | `false` | Fail `kargo run`, `kargo test` and `kargo task` before anything runs unless the shell or a `.env` file sets it. Builds don't check it |

Names are letters, digits and `_`, and don't start with a digit. Two keys are not variables; they hold arrays:

| Key | Meaning |
|---|---|
| `env-files` | More `.env` files, relative to the package root, read after the standard `.env` files and before `--env-file`. A missing one is an error |
| `expose` | Variables compiled into a generated `BuildConfig` object in the package, one `const val` for each one that is set. Only in `[env]`. Unset ones become a nullable `null` property. Applies to the JVM, Native and Wasm (Node.js) targets, not Android or Wasm browser apps |

`.env` files and the precedence between sources are described in [Environment variables](../../guides/environment/).

## `[workspace]`

A workspace's `kargo.toml` has only a `[workspace]` table, no `[package]`. Its members share one `kargo.lock`, one `target/` and one Kotlin version.

```toml
[workspace]
members = ["apps/*", "libs/core"]    # required: member directories or globs, relative to this file
exclude = ["apps/attic"]             # directories the globs must not match
default-members = ["apps/server"]    # what commands work on at the root (default: every member)

[workspace.package]                  # [package] keys members inherit with `key.workspace = true`
version = "1.2.0"
group = "com.example"

[workspace.kotlin]
version = "2.4.21"                   # the Kotlin version of every member
opt-in = ["kotlin.RequiresOptIn"]
jvm-target = "17"

[workspace.dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
core = { path = "libs/core" }        # paths are relative to the workspace root

[workspace.frameworks]
serialization = { formats = ["json"] }

[workspace.plugins.notify]
command = "tools/notify.sh"          # relative to the workspace root
hooks = ["post-build"]

[workspace.tasks]
release-notes = "./tools/notes.sh"   # `kargo release-notes` at the root
```

| Key | Meaning |
|---|---|
| `members` | Member directories, or globs such as `libs/*` (`*` and `?` within one path segment). A glob matches the directories that have a `kargo.toml`, except hidden ones. Required; may be empty in a new workspace |
| `exclude` | Directories that `members` globs must not match |
| `default-members` | The members `build`, `check`, `test` and the other commands work on at the workspace root, and the one `run` picks when there is only one. Default: every member |
| `[workspace.package]` | `version`, `group`, `description`, `authors`, `license`, `homepage` and `repository`, which members inherit with `key.workspace = true` |
| `[workspace.kotlin]` | `version`, `language-version`, `api-version`, `ksp`, `opt-in`, `compiler-args`, `warnings-as-errors` and `jvm-target` (the default `[target.jvm] jvm-target`) for every member. A member's own `opt-in` and `compiler-args` are added to these; its other `[kotlin]` keys take precedence, except `version`, which must match |
| `[workspace.dependencies]` | Dependency entries members inherit with `name = { workspace = true }` |
| `[workspace.frameworks]` | `[frameworks]` entries members inherit with `name = { workspace = true }` |
| `[workspace.plugins]` | `[plugins]` entries members inherit with `name = { workspace = true }`. A relative `command` stays relative to the workspace root |
| `[workspace.tasks]` | Tasks that run at the workspace root. Members inherit them with `name = { workspace = true }`, which runs them in the member's directory |

A member inherits a key with `{ workspace = true }` (or the dotted `key.workspace = true`), and nothing else in the same entry, except for `[dependencies]` entries, which may add `scope`, `exported` and `test-fixtures`:

```toml title="apps/server/kargo.toml"
[package]
name = "server"
version.workspace = true
group.workspace = true

[frameworks]
serialization.workspace = true

[plugins]
notify.workspace = true

[tasks]
release-notes.workspace = true
```

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
| `optimize` | `false` | `true` | Optimize (mainly affects Native and Wasm; for Wasm it removes dead code, see [Release builds](../../guides/wasm/#release-builds)) |
| `compiler-args` | `[]` | `[]` | Extra compiler arguments for this profile |
| `env` | `{}` | `{}` | `[profile.<name>.env]`: variables that replace `[env]` for this profile, one by one, and an `env-files` list of its own. Same format as `[env]`, but without `expose` |
