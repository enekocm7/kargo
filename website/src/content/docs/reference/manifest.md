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

`version` names the jar and is the version in its Maven coordinates, so it must be one exact version: a Maven range such as `[1.0,2.0)`, a wildcard such as `1.+`, a value with spaces and an empty string are errors (a `+` in the middle, as in `1.0.0+build.5`, is build metadata and fine). Kargo also [warns](../warnings/#odd-package-versions) about one that doesn't start with a digit or that uses anything besides letters, digits and `.`, `-`, `_`, `+`.

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

### `[package.jlink]`

How `kargo package --format jlink` (and `--format installer`) trims the Java runtime. See [Runtime images](../../guides/distributing/#runtime-images).

```toml
[package.jlink]
modules = ["java.naming", "jdk.crypto.ec"]
jdeps = true
```

| Key | Default | Meaning |
|---|---|---|
| `modules` | `[]` | JDK modules to add to the ones `jdeps` finds, such as modules the program only loads through reflection or a service loader |
| `jdeps` | `true` | Ask `jdeps` which modules the program and its dependencies use. `false` puts only `modules` in the runtime, so it then needs at least one |

### `[package.optimize]`

R8 settings for shrinking JVM jars. `kargo package --release` enables this automatically; pass `--optimize` to enable it with another profile. The minimized jar, mapping and removed-code report are written under `target/<profile>/jvm/`.

```toml
[package.optimize]
shrink = true
optimize = true
obfuscate = true
rules = "proguard-rules.pro"
```

| Key | Default | Meaning |
|---|---|---|
| `shrink` | `true` | Remove unreachable classes and members |
| `optimize` | `true` | Apply bytecode optimizations |
| `obfuscate` | `true` | Rename classes and members |
| `rules` | `"proguard-rules.pro"` | ProGuard-compatible rules file; an absent default file is ignored |

R8 is downloaded from Google's Maven repository and pinned with a checksum in `kargo.lock`. Kargo preserves the selected `[[bin]]` or package main entry point, and keeps public library APIs and Kotlin metadata for library jars. See [JVM shrinking](../../guides/publishing/#jvm-shrinking-and-obfuscation).

### `[package.native-image]`

Options for `kargo package --format native-image`. See [Native executables](../../guides/distributing/#native-executables).

```toml
[package.native-image]
args = ["--enable-url-protocols=https", "-march=native"]
static = false
initialize-at-build-time = ["kotlinx.serialization"]
metadata-repository = true
```

| Key | Default | Meaning |
|---|---|---|
| `args` | `[]` | Extra `native-image` arguments |
| `static` | `false` | Link a fully static executable against musl (`--static --libc=musl`). Linux only, and needs musl's `x86_64-linux-musl-gcc` on `PATH` |
| `initialize-at-build-time` | `[]` | Packages or classes to initialize while the image is built (`--initialize-at-build-time`) |
| `metadata-repository` | `true` | Use the [GraalVM reachability metadata repository](https://github.com/oracle/graalvm-reachability-metadata) for the dependencies it covers. Downloaded once into `~/.kargo/toolchains` |

### `[package.installer]`

Options for `kargo package --format installer` and `kargo package --desktop`, which run `jpackage`. See [Installers](../../guides/distributing/#installers). On macOS, `kargo run` of a Compose application also shows `name` and `icon` in the Dock.

```toml
[package.installer]
name = "Hello"                       # default: the package name (or --bin)
vendor = "Example Inc."
description = "Says hello"           # default: [package] description
app-version = "1.2.3"                # default: the numbers at the start of [package] version
type = ["msi", "dmg", "deb"]         # or one type; default: msi on Windows, dmg on macOS, deb on Linux
icon = { windows = "packaging/app.ico", macos = "packaging/app.icns", linux = "packaging/app.png" }
license-file = "LICENSE"
menu = true
shortcut = true
console = false
upgrade-uuid = "0b5b2d6e-8f5c-4a37-9d1e-3a6c2f4b7e90"
bundle-id = "com.example.hello"
signing-identity = "Developer ID Application: Example Inc. (TEAMID)"
category = "utils"
file-associations = [{ extension = "note", mime-type = "application/x-note", description = "Note", icon = "packaging/note.ico" }]
notarize = { apple-id-env = "APPLE_ID", password-env = "APPLE_APP_PASSWORD", team-id = "ABCDE12345" }
```

| Key | OS | Meaning |
|---|---|---|
| `name` | all | The application's name, its launcher's and its installer's. Must be usable as a file name |
| `vendor` | all | The vendor shown by the installer |
| `description` | all | The description shown by the installer |
| `app-version` | all | One to three numbers, such as `1.2.3`, which every installer format accepts. Defaults to the numbers at the start of `[package] version` (`1.2.3-SNAPSHOT` gives `1.2.3`) |
| `type` | all | `msi` or `exe` (Windows), `dmg` or `pkg` (macOS), `deb` or `rpm` (Linux), or `app-image`: the application directory without an installer, on any OS. A list builds the types the host OS builds and skips the others; it is an error when it has none for the host |
| `icon` | all | One path for every OS, or a table with `windows` (`.ico`), `macos` (`.icns`) and `linux` (`.png`) paths, relative to the project root |
| `license-file` | all | A license the installer shows |
| `menu` | Windows, Linux | Add a Start menu (Windows) or desktop menu (Linux) entry |
| `shortcut` | Windows, Linux | Add a desktop shortcut |
| `console` | Windows | Make the launcher a console program, so a command-line tool's output shows in the terminal. Without it the launcher opens no console window |
| `upgrade-uuid` | Windows | The upgrade code that lets a newer `.msi` replace an older one. Keep it the same across releases |
| `bundle-id` | macOS | The bundle identifier, such as `com.example.hello` |
| `signing-identity` | macOS | Sign the app with this identity from the keychain |
| `category` | Linux | The package's section or group (`--linux-app-category`) |
| `file-associations` | all | File types the installed application opens: a list of `{ extension, mime-type, description, icon }` tables (`description` and `icon` optional). Not registered by `app-image` |
| `notarize` | macOS | Notarize each `.dmg` and `.pkg` and staple the ticket: `{ apple-id-env, password-env, team-id }`, naming the environment variables that hold the Apple ID and the app-specific password, or `{ keychain-profile }` for credentials saved by `xcrun notarytool store-credentials`. Needs `signing-identity`; ignored on other OSes |

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
| `version` | `2.4.21` | Kotlin compiler and standard library version. One exact version: ranges such as `[2.0,3.0)` and wildcards such as `2.+` are errors |
| `language-version` | | `-language-version` |
| `api-version` | | `-api-version` |
| `ksp` | `2.3.12` | [KSP](../../guides/dependencies/#kotlin-symbol-processing-ksp) version for `scope = "ksp"` dependencies (the standalone tool on the JVM, the Gradle plugin on Android). One exact version |
| `opt-in` | `[]` | Annotations to opt in to everywhere |
| `compiler-args` | `[]` | Extra arguments for every compilation |
| `warnings-as-errors` | `false` | Fail on warnings. The compiler's `warnings found and -Werror specified` summary is printed [after the diagnostics](../warnings/#warnings-as-errors) |

In a workspace, `[workspace.kotlin]` sets these for every member: a member's `opt-in` and `compiler-args` are added to the workspace's, and its other keys take precedence (see [`[workspace]`](#workspace)).

## `[toolchain]`

```toml
[toolchain]
jdk = "21"         # "system" (default), a major version, or an exact release like "21.0.5+11"
graalvm = "25"     # "system" (default), a Java version for GraalVM CE, or "oracle-25" for Oracle GraalVM
android-sdk = "36" # "system" (default), or a platform API level that Kargo installs
kargo = ">=0.3"    # the Kargo versions this project builds with (default: any)
```

`graalvm` is the GraalVM that `kargo package --format native-image` and `kargo run --native-agent` use. `system` looks at `GRAALVM_HOME`, then a `JAVA_HOME` with `native-image`, then `native-image` on `PATH`.

`android-sdk` is the Android SDK of the Android target. An API level such as `"36"` (or `36`; 21 or newer) makes Kargo install that platform, the build-tools of the Android Gradle Plugin in use and platform-tools under `~/.kargo/toolchains/android-sdk`, after you accept Google's license terms, and build with that SDK. `compile-sdk` under [`[target.android]`](#targetandroid) defaults to it and must equal it when set. `system` uses `ANDROID_HOME`, then `ANDROID_SDK_ROOT`, then Android Studio's default location. See [Android SDK](../../guides/android/#android-sdk).

`kargo` is a [semver requirement](https://docs.rs/semver/latest/semver/enum.Op.html) such as `">=0.3"`, `"^0.3.1"` or `">=0.3, <0.5"`. A Kargo that does not satisfy it stops before doing anything else with the project, and says how to get a newer one:

```console
$ kargo build
error: this project needs kargo >=0.3 (`[toolchain] kargo` in kargo.toml), but this is kargo 0.1.0: run `kargo self update` to get a newer Kargo
```

[`kargo self`](../commands/#kargo-self) ignores it, so an old Kargo can always update itself. A Kargo older than this key rejects it as an unknown key, so it protects against old versions only from the release that introduced it on.

See [Managing JDKs](../../guides/toolchains/) and [GraalVM](../../guides/toolchains/#graalvm).

## `[build]`

How the commands that build run. Every key is optional.

```toml
[build]
jobs = 2
default-targets = ["jvm", "wasm"]
```

| Key | Default | Meaning |
|---|---|---|
| `default-targets` | every enabled target | The targets `kargo build`, `check`, `test`, `bench`, `doc` and `fetch` use when you don't pass `-t`: any of `jvm`, `wasm`, `js`, `native` and `android`. The manifest keeps all its `[target.*]` sections, so `-t android` still builds Android. Each one must be enabled in the package, and the list can't be empty |
| `jobs` | `"auto"` | How many workspace members, and targets and Native platforms of each, build at the same time: a number, or `"auto"` (also `0`) for the number of CPUs within the free memory. `-j` and `KARGO_JOBS` override it, and it overrides `[build] jobs` of `config.toml`. `kargo test`, `bench`, `doc`, `fmt` and `lint` handle one member at a time when no number of jobs is set |

In a workspace, write `jobs` as `[workspace.build]` in the root `kargo.toml`; a member's own `jobs` applies only when the member builds on its own. See [Limiting parallelism](../../guides/build-performance/#limiting-parallelism).

`default-targets` in `[workspace.build]` applies to every member that enables at least one of the listed targets (the others build all of theirs), so a workspace that enables Android and iOS in some members can build only the JVM and Wasm by default. A member's own `[build] default-targets` takes precedence over the workspace's. Where one is set, `-t` still picks any enabled target:

```toml title="kargo.toml"
[workspace.build]
default-targets = ["jvm", "wasm"]
```

```sh
kargo build -p app            # jvm and wasm
kargo build -p app -t android # only android
```

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
| `timeout` | none | How long the whole test run may take, such as `"30s"`, `"5m"` or `"1h"`, counted from when the tests start running. The run is stopped and fails when it passes. Applies to the JVM, Native, Wasm and JS targets, not Android |
| `test-timeout` | none | How long one JVM test may take, such as `"30s"`. A test that runs longer fails, and the run goes on. Applies to tests run by JUnit Jupiter |
| `jvm-args` | `[]` | JVM arguments for the test process, after `[target.jvm] jvm-args` and the profile's [`jvm-args`](#profilename) |

See [Testing every target](../../guides/testing/#testing-every-target) and [Timeouts](../../guides/testing/#timeouts).

## `[bench]`

Options for `kargo bench`. Every key is optional.

```toml
[bench]
warmup = "5x1s"
iterations = "5x1s"
forks = 2
mode = "avgt"
time-unit = "us"
jvm-args = ["-Xmx2g"]
```

| Key | Default | Meaning |
|---|---|---|
| `warmup` | `"3x1s"` | Warmup iterations: a count, `x`, and how long each lasts (`ms`, `s`, `m` or `h`), such as `"5x1s"` or `"3x500ms"` |
| `iterations` | `"5x1s"` | Measured iterations, in the same format |
| `forks` | `2` | How many JVMs run each benchmark, one after the other. `0` runs it in the JVM of the launcher, which is faster and less reliable |
| `mode` | `"avgt"` | What to measure: `thrpt` (operations per time unit), `avgt` (time per operation), `sample` (time per operation, sampled, with percentiles), `ss` (one operation, cold) or `all` |
| `time-unit` | best fit | The unit results are reported in: `ns`, `us`, `ms`, `s`, `m`, `h` or `d`. Without one, each score is shown in whichever unit reads best |
| `jvm-args` | `[]` | JVM arguments for the benchmark JVMs, after `[target.jvm] jvm-args` and the profile's [`jvm-args`](#profilename) |

A key you leave out is taken from the JMH annotation on the benchmark (`@Warmup`, `@Measurement`, `@Fork`, `@BenchmarkMode`, `@OutputTimeUnit`) when the sources of `benches/` use it, and otherwise has the default above. The `kargo bench` flags of the same names win over `[bench]`.

See [Benchmarking](../../guides/benchmarking/#settings).

## `[audit]`

Options for [`kargo audit`](../commands/#kargo-audit). Every key is optional.

```toml
[audit]
deny = "medium"
ignore = [
    { id = "GHSA-xxxx-xxxx-xxxx", reason = "we never parse untrusted YAML" },
    { id = "CVE-2024-1234", reason = "waiting for the 2.1 release", until = 2026-12-31 },
]
```

| Key | Default | Meaning |
|---|---|---|
| `deny` | `"high"` | The lowest severity that makes `kargo audit` fail with exit code 101: `low`, `medium`, `high`, `critical`, or `none` to report without failing. `kargo audit --deny` overrides it |
| `ignore` | `[]` | Advisories that do not apply to this project |

Each `ignore` entry is a table:

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | The advisory's id or any of its aliases, such as `GHSA-...` or `CVE-...` (case does not matter) |
| `reason` | yes | Why the advisory does not apply; `kargo audit` prints it next to the advisory it ignores |
| `until` | no | The last day the entry applies, as a TOML date (`2026-12-31`) or the same as a string. Afterwards the advisory is reported again, with a warning |

An ignored advisory is left out of the report and of the exit code, but still in `--format json` (with its `ignored` reason). An entry that matches no advisory of the audited packages gets a note, so stale entries do not pile up.

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
opt-level = "balanced"   # "size" (-Oz), "balanced" (-O2) or "speed" (-O3)
wasm-opt = true            # pinned Binaryen is installed on demand; false or a custom path also work
wasm-opt-flags = []        # extra wasm-opt passes/options
debug-info = false         # keep names and write a .wasm.map in release builds
compress = ["gzip", "brotli"] # emit .gz/.br copies of the module and JS glue
content-hash = false       # include a content hash in module filenames
max-size = "1 MiB"         # fail a release build above this Wasm size
max-gzip-size = "256 KiB"  # fail a release build above this gzipped size
size-report = false        # print Binaryen per-function metrics
wasm-enable-array-range-checks = false
wasm-debugger-custom-formatters = false
web-resources = ["web/raylib.mjs", "web/raylib.wasm"]  # a library's files for the browser apps using it
web-init = "web/init.mjs" # a library's script that runs before a dependent app's module
js-sources = ["js"]       # your own JavaScript and TypeScript modules; default: js/
port = 5173               # the port `kargo run` serves a browser app on; default 8080

[target.wasm.npm-dependencies]
"left-pad" = "1.3.0"      # npm packages besides those your Wasm libraries ask for

[target.wasm.dev-npm-dependencies]
mocha = "^10.0.0"         # npm packages only `kargo test` installs
```

| Key | Default | Meaning |
|---|---|---|
| `environment` | `"js"` | `"js"` for Node.js, `"browser"` for web pages (`kargo run` serves them and `kargo test` uses a headless browser), `"wasi"` for WASI runtimes. See [WebAssembly](../../guides/wasm/#setup) |
| `kind` | `"application"` | `"application"` for a program with a `main`, `"library"` for an npm package of `@JsExport` functions with TypeScript declarations. A library needs `environment = "js"` or `"browser"` |
| `module-name` | the package name | Name of the compiled module's files (`<name>.mjs`, `<name>.wasm`). Letters, digits, `-`, `_` and `.` |
| `npm-name` | the lowercase package name | `name` in the `package.json` of a library. May be scoped (`@scope/name`); only valid with `kind = "library"` |
| `package-manager` | first of `npm`, `pnpm`, `bun` on `PATH` | The tool that installs the [npm packages](../../guides/wasm/#npm-packages) your project needs |
| `wasm-opt` | `true` | In release builds, use the pinned Binaryen `wasm-opt` installed on demand. `false` disables it; a string is a custom executable path relative to the project. |
| `opt-level` | `"balanced"` | Binaryen optimization preset: `size` (`-Oz`), `balanced` (`-O2`) or `speed` (`-O3`). |
| `wasm-opt-flags` | none | Additional arguments passed to Binaryen after Kargo's required Wasm feature flags. |
| `debug-info` | `false` | Keep debug names and generate a separate `.wasm.map` in release output. |
| `compress` | none | Emit `.gz` and/or `.br` files for the Wasm module and JavaScript glue; the browser server negotiates `Content-Encoding`. |
| `content-hash` | `false` | Add a hash of the Wasm bytes to the module and Wasm filenames, and update generated references. |
| `max-size` | none | Fail release builds when the Wasm module exceeds a byte size such as `"1 MiB"`. |
| `max-gzip-size` | none | Fail release builds when the gzipped Wasm module exceeds the configured size. |
| `size-report` | `false` | Ask Binaryen for module and per-function metrics. The `kargo build --size-report` flag enables this for one invocation. |
| `wasm-enable-array-range-checks` | profile default | Override Kotlin/Wasm array range checks; Kargo disables them in optimized builds by default. |
| `wasm-debugger-custom-formatters` | `false` | Enable Kotlin/Wasm custom formatters in browser debuggers. |
| `npm-dependencies` | none | A table of npm packages (`name = "version"`, a semver range such as `^1.3.0`) to install in addition to those the klibs ask for. Your version wins on a conflict. Each gets [Kotlin declarations](../../guides/wasm/#kotlin-declarations-for-npm-packages) generated from its TypeScript ones. [`kargo add --npm`](../commands/#npm-packages-with-kargo-add---npm) edits it |
| `dev-npm-dependencies` | none | npm packages only the tests need. `kargo test` installs them (and the `npm-dependencies`) next to the test module; no Kotlin declarations are generated for them. `kargo add --npm --dev` edits it |
| `web-resources` | none | Files and directories of a library, relative to the package, that every browser app depending on it gets next to its module under their own names (a directory keeps its name). Published as a `web-resources` zip. Needs `kind = "library"` and `environment = "js"` or `"browser"`. See [Web resources from libraries](../../guides/wasm/#web-resources-from-libraries) |
| `web-init` | none | A script of a library, relative to the package, that the page Kargo generates for a dependent browser app imports before the app module, after the init scripts of the libraries it depends on. It is shipped like `web-resources`. Same requirements as `web-resources` |
| `js-sources` | `["js"]` | Directories, relative to the package, of your own JavaScript and TypeScript modules. They land in `js/` next to the module (TypeScript compiled with tsc) and get Kotlin declarations in the package `js.<path>`. A directory that doesn't exist is skipped; `[]` turns the convention off. Not for `environment = "wasi"`. See [Your own JavaScript and TypeScript](../../guides/wasm/#your-own-javascript-and-typescript) |
| `port` | `8080` | The port `kargo run` serves a browser app on, `0` for a free one. `kargo run --port` overrides it. Needs `environment = "browser"`. See [Choosing the port](../../guides/wasm/#choosing-the-port) |

With `optimize = true`, as in the [release profile](#profilename), a Wasm build removes code nothing reaches, runs pinned Binaryen `wasm-opt` and reports module, gzip and Brotli sizes. Size budgets are checked after optimization.

### `[target.js]`

```toml
[target.js]
environment = "node"      # "node" (default) or "browser"
kind = "application"      # "application" (default) or "library"
module-kind = "es"        # "es" (default), "commonjs" or "umd"
module-name = "hello"     # name of the compiled files; default: the package name
npm-name = "@acme/hello"  # npm package name of a library; default: the lowercase package name
package-manager = "pnpm"  # "npm", "pnpm" or "bun"; default: the first one found on PATH
js-sources = ["js"]       # your own JavaScript and TypeScript modules; default: js/
port = 5173               # the port `kargo run` serves a browser app on; default 8080

[target.js.npm-dependencies]
"left-pad" = "1.3.0"      # npm packages besides those your Kotlin/JS libraries ask for

[target.js.dev-npm-dependencies]
mocha = "^10.0.0"         # npm packages only `kargo test` installs
```

| Key | Default | Meaning |
|---|---|---|
| `environment` | `"node"` | `"node"` for Node.js, `"browser"` for web pages (`kargo run` serves them and `kargo test` uses a headless browser, as for Wasm). See [JavaScript](../../guides/js/#setup) |
| `kind` | `"application"` | `"application"` for a program with a `main`, `"library"` for an npm package of `@JsExport` declarations with TypeScript declarations and a `package.json` |
| `module-kind` | `"es"` | The JavaScript module the compiler writes: `"es"` (`<name>.mjs`), `"commonjs"` or `"umd"` (`<name>.js`). A `browser` application can't use `"commonjs"`. See [Module kinds](../../guides/js/#module-kinds) |
| `module-name` | the package name | Name of the compiled module's files. Letters, digits, `-`, `_` and `.` |
| `npm-name` | the lowercase package name | `name` in the `package.json` of a library. May be scoped (`@scope/name`); only valid with `kind = "library"` |
| `package-manager` | first of `npm`, `pnpm`, `bun` on `PATH` | The tool that installs the [npm packages](../../guides/js/#npm-packages) the JS target needs |
| `npm-dependencies` | none | A table of npm packages (`name = "version"`, a semver range such as `^1.3.0`) to install in addition to those the klibs ask for. Your version wins on a conflict. Each gets [Kotlin declarations](../../guides/js/#npm-packages) generated from its TypeScript ones (Kotlin 2.2.20 or later). [`kargo add --npm`](../commands/#npm-packages-with-kargo-add---npm) edits it |
| `dev-npm-dependencies` | none | npm packages only the tests need, installed by `kargo test` next to the test module. No Kotlin declarations are generated for them. `kargo add --npm --dev` edits it |
| `js-sources` | `["js"]` | Directories of your own JavaScript and TypeScript modules, as for Wasm. See [Your own JavaScript and TypeScript](../../guides/js/#your-own-javascript-and-typescript) |
| `port` | `8080` | The port `kargo run` serves a browser app on, as for Wasm. Needs `environment = "browser"` |

With `optimize = true`, as in the [release profile](#profilename), the JS module is linked with dead code elimination and without a source map.

### `[target.native]`

```toml
[target.native]
platforms = ["host"]               # default ["host"]
kind = "executable"                # "executable" (default), "library", "shared", "static" or "framework"
entry-point = "com.example.main"
linker-options = ["-lcurl"]
library-paths = ["vendor/lib"]     # -L directories, relative to the project root
static-libraries = ["libextra.a"]  # found in library-paths, linked in
frameworks = ["Foundation"]        # Apple platforms only
bundle-id = "com.example.App"      # kind = "framework" only; default <group>.<name>
export = ["com.example:core"]      # dependencies whose API a framework or C library exports

[target.native.platform.mingw_x64] # more linker options for one platform
linker-options = ["-lws2_32"]

[target.native.c-sources]          # C/C++ files compiled into a static library and linked in
sources = ["c"]                    # files, directories and globs; default ["c"]
include-dirs = ["c"]               # default ["c"]
defines = ["NDEBUG"]
compiler-options = ["-Wall"]

[target.native.c-sources.platform.linux_x64]
defines = ["LINUX"]

[target.native.cinterop.sqlite]    # a C library bound with cinterop; one table per library
def = "interop/sqlite.def"         # default: interop/<name>.def
package = "sqlite3"
headers = ["sqlite3.h"]
compiler-options = ["-I/usr/include"]
linker-options = ["-lsqlite3"]
pkg-config = "sqlite3"
link = "static"

[target.native.cinterop.sqlite.platform.macos_arm64]
linker-options = ["-lsqlite3"]
```

Valid platforms: `host`, `linux_x64`, `linux_arm64`, `mingw_x64`, `macos_x64`, `macos_arm64`, `ios_arm64`, `ios_x64`, `ios_simulator_arm64`, `watchos_arm64`, `watchos_simulator_arm64`, `tvos_arm64`, `tvos_simulator_arm64`, `android_arm64`, `android_arm32`, `android_x64`. Gradle-style names like `linuxX64` are accepted too.

| `[target.native]` key | Default | Meaning |
|---|---|---|
| `platforms` | `["host"]` | The platforms to build for |
| `kind` | `"executable"` | `"executable"`, `"library"` (a klib), `"shared"` and `"static"` (a C library with its header in `include/`), or `"framework"` (an Apple framework; every platform must be Apple's) |
| `entry-point` | the single `main` | The entry function of an executable |
| `linker-options` | `[]` | Options for the linker of the project's binaries |
| `library-paths` | `[]` | Directories passed as `-L` and searched for `static-libraries` |
| `static-libraries` | `[]` | Static libraries (`libfoo.a`) from `library-paths`, linked into the binaries |
| `frameworks` | `[]` | Apple frameworks to link; ignored on other platforms |
| `bundle-id` | `<group>.<name>` | The `CFBundleIdentifier` of a framework |
| `export` | `[]` | Dependencies (`group:artifact`) whose API a framework, shared or static library exports too |

`[target.native.platform.<platform>]` takes `linker-options`, `library-paths`, `static-libraries` and `frameworks`, added to the common ones for that platform. The platform must be named (`host` is not allowed).

| `[target.native.c-sources]` key | Default | Meaning |
|---|---|---|
| `sources` | `["c"]` | Files, directories (their `.c`, `.cc`, `.cpp`, `.cxx`, `.m` and `.mm` files) and globs, relative to the project root |
| `include-dirs` | `["c"]` | `-I` directories; every cinterop entry gets them too |
| `defines` | `[]` | Macros, `NAME` or `NAME=value` |
| `compiler-options` | `[]` | More clang options |

`[target.native.c-sources.platform.<platform>]` takes `defines` and `compiler-options`. See [Bundled C sources](../../guides/c-interop/#bundled-c-sources).

| `[target.native.cinterop.<name>]` key | Default | Meaning |
|---|---|---|
| `def` | `interop/<name>.def` | The `.def` file, relative to the project root. It must exist unless `headers` or `modules` are given |
| `package` | the `.def` file's `package` | Kotlin package of the generated bindings |
| `headers` | `[]` | Headers to bind |
| `compiler-options` | `[]` | clang options for reading the headers (`-I`, `-D`, ...) |
| `linker-options` | `[]` | Linker options recorded in the bindings and used by every program that links them |
| `library-paths` | `[]` | Directories searched for `static-libraries`, also passed as `-L` |
| `static-libraries` | `[]` | Static libraries (`libfoo.a`) embedded in the bindings |
| `frameworks` | `[]` | Apple frameworks to link; ignored on other platforms |
| `pkg-config` | none | A package name or a list of them; their flags apply on the host platform |
| `link` | `"dynamic"` | `"static"` embeds the `lib<name>.a` of each `-l` option found in a library directory |
| `header-filter` | `[]` | The `.def` file's `headerFilter` |
| `excluded-functions` | `[]` | The `.def` file's `excludedFunctions` |
| `strict-enums` | `[]` | The `.def` file's `strictEnums` |
| `non-strict-enums` | `[]` | The `.def` file's `nonStrictEnums` |
| `modules` | `[]` | The `.def` file's `modules` (Clang modules, such as Objective-C frameworks) |
| `language` | the `.def` file's | `"C"` or `"Objective-C"` |

`[target.native.cinterop.<name>.platform.<platform>]` takes `compiler-options`, `linker-options`, `library-paths`, `static-libraries` and `frameworks`. The name may contain letters, digits, `_` and `-`. See [C interop](../../guides/c-interop/).

### `[target.android]`

```toml
[target.android]
namespace = "com.example.app"    # default: <group>.<name>
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
| `namespace` | `<group>.<name>` of `[package]`, lowercase, with `_` for `-` and other characters a Java package can't have (`com.example.my_lib` for `my-lib`). The package of the `R` and `BuildConfig` classes. Required when `[package]` has no `group` |
| `application-id` | `namespace` |
| `kind` | `application`. A `library` builds an AAR, can be a [path dependency](../../guides/android/#android-libraries) of other Android packages and is [published](../../guides/publishing/#android-libraries) as an AAR |
| `min-sdk` | `24` |
| `target-sdk` | `36`, or `compile-sdk` when that is lower |
| `compile-sdk` | [`[toolchain] android-sdk`](#toolchain) when it is an API level (and must equal it when set), else `37` |
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

## `[[generate]]`

Steps that write sources or resources before they are compiled. Repeat the table for each step. See [Code generation](../../guides/code-generation/).

```toml
[[generate]]
name = "protos"
inputs = ["proto/**/*.proto"]
run = "protoc -Iproto --java_out=$KARGO_GEN_DIR proto/*.proto"
targets = ["jvm", "android"]

[[generate]]
name = "api-client"
inputs = ["openapi.yaml"]
tool = "org.openapitools:openapi-generator-cli:7.16.0"
args = ["generate", "-g", "kotlin", "-i", "openapi.yaml", "-o", "$KARGO_GEN_DIR"]
roots = ["src/main/kotlin"]       # the tool writes a whole project; compile only this directory

[[generate]]
name = "config"
builtin = "build-config"
fields = { API_URL = "https://api.example.com" }
```

A step does exactly one of `run`, `tool` and `builtin`.

| Key | Meaning |
|---|---|
| `name` | Required. Letters, digits, `-` and `_`, starting with a letter; unique. Names the output directory, `target/generated/<profile>/<name>/` (`$KARGO_GEN_DIR`) |
| `inputs` | Globs (gitignore syntax, relative to the project root) of the files the output comes from. The step runs only when their contents, its definition or Kargo changed. Without `inputs`, it runs on every build. Default for `protobuf`: `proto/**/*.proto` |
| `run` | A shell command or a list of them, run at the project root. As for [tasks](#tasks), `windows-run` replaces it on Windows and `shell` picks the shell |
| `windows-run`, `shell` | Only with `run` |
| `tool` | A Maven artifact, `group:artifact:version`. Kargo resolves it with its dependencies (locked in `kargo.lock` as `generate/<name>`) and runs its main class on the project's JDK |
| `main-class` | The class `tool` runs. Default: the `Main-Class` of the jar's manifest |
| `args` | The arguments of `tool` (or more arguments for `protoc`). `$NAME` and `${NAME}` are replaced with the step's variables, such as `$KARGO_GEN_DIR` |
| `builtin` | `build-config` or `protobuf` |
| `targets` | The targets that compile the output: `jvm`, `wasm`, `js`, `native`, `android`. Default: all of them; for `protobuf`, `jvm` and `android` |
| `kind` | `sources` (default), `test-sources` or `resources`. `resources` needs the `jvm` or `android` target |
| `roots` | The directories of the output, relative to it, that hold the sources or resources, such as `src/main/kotlin` for a tool that writes a whole Gradle project. Default: the whole output directory |
| `after` | Names of steps that run before this one. Steps without such a link run at the same time |
| `env` | Extra environment variables of the step |

The keys of the built-in generators:

| Key | Default | Meaning |
|---|---|---|
| `package` | the group and name of the package | `build-config`: the Kotlin package of the object |
| `object` | `BuildConfig` | `build-config`: the name of the object |
| `fields` | none | `build-config`: constants of your own; a string, integer or boolean |
| `expose` | none | `build-config`: variables of the project [environment](#env) to write as constants |
| `build-time` | `false` | `build-config`: also write `BUILD_TIME`, which changes on every build unless `SOURCE_DATE_EPOCH` is set |
| `version` | `4.31.1` | `protobuf`: the `protoc` version |
| `proto-path` | `["proto"]` | `protobuf`: the directories searched for imports (`-I`) |
| `java`, `kotlin` | `true` | `protobuf`: generate the Java classes and the Kotlin DSL (which needs `java`) |
| `grpc` | `false` | `protobuf`: also generate gRPC stubs |
| `grpc-java-version` | `1.73.0` | `protobuf`: the version of `protoc-gen-grpc-java` |
| `grpc-kotlin-version` | `1.4.3` | `protobuf`: the version of `protoc-gen-grpc-kotlin` |

`kargo generate` runs the steps on their own, and every command that compiles runs the ones it needs first. The output directory is empty when a step starts. A step may only use the keys of its kind: `package` with `run` is an error, for instance.

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

Maven Central is used first, and Google Maven is added automatically for Android and Compose. Your repositories come after them, in the order you write them.

| Key | Default | Meaning |
| --- | --- | --- |
| `<name>` | | A repository: its URL (`https://`, `http://` or `file:`), `"maven-local"`, or a table with the keys below |
| `<name>.url` | | The URL. Leave it out for `maven-central` and `google` to configure the built-in repository |
| `<name>.username-env`, `<name>.password-env` | none | Environment variables holding the user name and password (or token), sent with HTTP Basic authentication |
| `<name>.include-groups` | all groups | Groups this repository serves. No other repository is asked for them |
| `<name>.exclude-groups` | none | Groups this repository is never asked for |
| `defaults` | `true` | `false` leaves out Maven Central and Google Maven, so only the repositories you list are used |

A group pattern is a group, `"com.corp"`, or a group and every group below it, `"com.corp.*"` (which also matches `com.corp`). Use them to keep private libraries private:

```toml
[repositories.corp]
url = "https://nexus.corp/maven"
username-env = "NEXUS_USER"
password-env = "NEXUS_PASSWORD"
include-groups = ["com.corp.*"]      # only from here, and never asked of Maven Central

[repositories.maven-central]
exclude-groups = ["com.example.unwanted"]
```

A group that a repository lists in `include-groups` is looked up in that repository only, so its coordinates never reach a public repository, and a higher version published there can't replace yours (dependency confusion). Changing these keys re-resolves `kargo.lock`. See [Choosing which repository serves a library](../../guides/dependencies/#choosing-which-repository-serves-a-library).

Credentials sent over plain `http://` to another machine are a warning, and an error with `--locked`. Use `https://`.

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
compose = true            # or { version, material3 = true, resources-package, public-resources = false, stability-config, strong-skipping = true, feature-flags = [], metrics = false, reports = false }
ktor = { server = "netty", client = "cio", client-engines = { native = "darwin", wasm = "none" }, server-plugins = [], client-plugins = [], json = true, version = "3.6.0" }
spring-boot = true        # or { version, starters = ["web"], jpa = false }
lombok = true             # or { version, config = "lombok.config" }
benchmark = true          # or { version = "0.5.0" }
```

Each can also be `false`. Default versions: serialization 1.11.0, coroutines 1.11.0, Compose 1.12.1 (Material 3 1.9.0), Ktor 3.6.0, Spring Boot 4.1.1, Lombok 1.18.48, kotlinx-benchmark 0.5.0 (with JMH 1.37).

For Ktor, `server` and `client` are the names of an engine (`client = true` is an error). `client` adds its engine to every target; `client-engines` sets the engine of single targets (`jvm`, `android`, `wasm`, `js`, `native`) or `"none"` for `ktor-client-core` alone. The [frameworks reference](../frameworks/) lists the keys of every framework and the artifacts each target gets.

For Compose, `resources-package` is the package of the `Res` class generated from `composeResources/` (default `<group>.<name>.generated.resources`, in lower case), and `public-resources = true` makes `Res` and its accessors `public` instead of `internal`. See [Compose resources](../../guides/compose/#resources).

The other Compose keys are options of the Compose compiler, on the JVM and Android targets:

| Key | Default | Meaning |
| --- | --- | --- |
| `stability-config` | none | A stability configuration file, or a list of them, relative to the package |
| `strong-skipping` | `true` | Strong skipping mode |
| `feature-flags` | `[]` | `StrongSkipping`, `IntrinsicRemember`, `OptimizeNonSkippingGroups`, `PausableComposition`; a leading `-` disables one. Don't combine `StrongSkipping` with `strong-skipping` |
| `metrics` | `false` | Write the compiler's metrics to `target/compose/<target>/metrics/` |
| `reports` | `false` | Write the compiler's stability reports to `target/compose/<target>/reports/` |

See [Compose compiler options](../../guides/compose/#compiler-options).

Compose works on the jvm (desktop) and android targets, on wasm with `environment = "browser"`, and on native with iOS `platforms` (`ios_arm64`, `ios_simulator_arm64`, `ios_x64`) and `kind = "framework"`. Other Wasm environments and Native platforms are errors, and so is the js target (Compose for Web runs on Kotlin/Wasm). See [Compose](../../guides/compose/).

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
| `expose` | Variables compiled into a generated `BuildConfig` object in the package, one `const val` for each one that is set. Only in `[env]`. Unset ones become a nullable `null` property. Applies to the JVM, Native, and Wasm and JS (Node.js) targets, not Android or browser apps |

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

[workspace.build]
jobs = 2                             # members and targets built at the same time
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
| `[workspace.build]` | [`[build]`](#build) for the whole workspace: `jobs` and `default-targets` |

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

## `[profile.<name>]`

`dev` (the default) and `release` (`--release`) always exist. Any other name is a custom profile, selected with `--profile <name>`. A custom profile starts from the settings of the profile it `inherits` from, and sets only what differs:

```toml
[profile.release]
debug = false
optimize = true
compiler-args = ["-Xno-call-assertions"]

[profile.staging]
inherits = "release"
debug = true
jvm-args = ["-Xmx1g", "-Dapp.env=staging"]

[profile.staging.dependencies]
"com.squareup.leakcanary:leakcanary-android" = "2.14"
```

```console
$ kargo build --profile staging
$ kargo run --profile staging
```

| Key | `dev` default | `release` default | Custom profile default | Meaning |
|---|---|---|---|---|
| `inherits` | not allowed | not allowed | `"dev"` | The profile this one starts from: `dev`, `release` or another custom profile. Keys it does not set come from there, through any number of steps |
| `debug` | `true` | `false` | inherited | Include debug information |
| `optimize` | `false` | `true` | inherited | Optimize (mainly affects Native, Wasm and JS; for Wasm and JS it removes dead code, see [Release builds](../../guides/wasm/#release-builds)) |
| `compiler-args` | `[]` | `[]` | inherited | Extra compiler arguments for this profile |
| `jvm-args` | `[]` | `[]` | inherited | JVM options for `kargo run`, `kargo test` and `kargo bench`, after `[target.jvm] jvm-args` (`kargo test` puts `[test] jvm-args` last, `kargo bench` puts `[bench] jvm-args` last) |
| `env` | `{}` | `{}` | inherited | `[profile.<name>.env]`: variables that replace the inherited ones of the same name, and an `env-files` list added to the inherited list. Same format as `[env]`, but without `expose` |
| `dependencies` | `{}` | `{}` | inherited | `[profile.<name>.dependencies]`: dependencies only this profile's builds have, in the format of [`[dependencies]`](#dependencies-and-dev-dependencies). One with the same artifact as an inherited one replaces it |

A custom profile's name is a letter, then letters, digits, `-` or `_`. Its output goes to `target/<name>/<target>/`, and its variables come from `.env.<name>` files as well (see [Environment](../../guides/environment/)). `debug` is the output of `dev`, and `android` and `lint` belong to other things under `target/`, so these names are reserved.

Path dependencies are not allowed in a profile. A profile's dependencies are not inherited by the packages that depend on yours. In a [workspace](../../guides/workspaces/), every member you build with `--profile` must define that profile.

Dependencies of a profile are part of every target's build with that profile, and `kargo.lock` resolves them with the others. A profile that has dependencies of its own gets a separate resolution there (for example `jvm/runtime@staging`), so the profiles without dependencies keep sharing `jvm/runtime`. See [Lockfile](../lockfile/#what-it-records). On Android, the dependencies of `dev` and `release` go to the `debug` and `release` build types (`debugImplementation`, `releaseImplementation`); a custom profile does not build for Android yet.
