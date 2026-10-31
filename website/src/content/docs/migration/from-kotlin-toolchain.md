---
title: Migrating from the Kotlin Toolchain
description: Move an Amper or Kotlin Toolchain project to kargo.toml, step by step.
---

The [Kotlin Toolchain](https://kotlin-toolchain.org/) (formerly Amper) is JetBrains' build tool for Kotlin projects. It replaces Gradle scripts with YAML: a `project.yaml` lists the modules, each module has a `module.yaml`, and one `kotlin` command (the wrapper scripts `kotlin` and `kotlin.bat`, previously `amper`) builds, tests, runs and publishes them.

Kargo is close in spirit: one declarative file, one command, no Gradle for the JVM, Wasm, JS and Native. The differences that matter when you move:

| | Kotlin Toolchain | Kargo |
|---|---|---|
| Written in | Kotlin and the JVM | Rust, one binary |
| Project file | `project.yaml` and one `module.yaml` per module | One `kargo.toml` per package, `[workspace]` for several |
| Dependencies | Maven coordinates, version catalogs and toolchain catalogs | The same, with full POM inheritance, BOMs, exclusions, version ranges and Gradle module metadata |
| Versions | Resolved on every build | `kargo.lock` pins every version and the SHA-256 of every artifact and tool |
| JVM, Wasm, JS, Native | Built directly, without Gradle | Built directly, without Gradle |
| Android | Android Gradle Plugin | Android Gradle Plugin, with the Gradle project generated from `kargo.toml` |
| Extensibility | Kotlin build plugins and templates | `kargo-<name>` subcommands, `[tasks]`, hooks and [`[[generate]]`](../guides/code-generation/) steps |
| Wrapper | `kotlin` / `kotlin.bat` with a pinned toolchain version | `[toolchain] kargo` sets the minimum Kargo version; `kargo self update` updates it |

:::tip
`kargo import` reads `project.yaml`, every `module.yaml` and the project's version catalog, writes the `kargo.toml` files and moves the sources into [Kargo's layout](../getting-started/project-layout/). Read [Importing a project](../guides/importing/#kotlin-toolchain) first, then use this guide for the parts its report points at.
:::

## Step 1: Import or translate `module.yaml`

```sh
cd my-project
kargo import --dry-run     # print every kargo.toml and every file move
kargo import --no-build    # write them and move the sources
```

For a hand-written translation, start from a module:

```yaml title="shared/module.yaml"
product:
  type: kmp/lib
  platforms: [ jvm, android, iosArm64 ]

dependencies:
  - io.ktor:ktor-client-core:3.6.0
  - $compose.foundation: exported

dependencies@jvm:
  - io.ktor:ktor-client-java:3.6.0

settings:
  kotlin:
    version: 2.4.21
    serialization: json
  compose: enabled
  jvm:
    jdk:
      version: 21
  android:
    namespace: com.example.shared
```

The Kargo equivalent:

```toml title="kargo.toml"
[package]
name = "shared"
version = "0.1.0"

[kotlin]
version = "2.4.21"

[toolchain]
jdk = "21"

[target.jvm]

[target.android]
kind = "library"
namespace = "com.example.shared"

[target.native]
platforms = ["iosArm64"]
kind = "library"

[frameworks]
serialization = true
compose = true

[dependencies]
"io.ktor:ktor-client-core" = "3.6.0"

[target.jvm.dependencies]
"io.ktor:ktor-client-java" = "3.6.0"
```

`settings.compose` and `settings.kotlin.serialization` become [frameworks](../guides/frameworks/): Kargo adds the libraries, compiler plugins and compiler options the framework needs, so the `$compose` and `$kotlin.serialization` dependencies go away.

## Step 2: Move the sources

Kargo's directories are flat and carry the target as a suffix:

| Kotlin Toolchain | Kargo |
|---|---|
| `src` | `src/` |
| `src@jvm` | `src-jvm/` |
| `src@android` | `src-android/` |
| `src@js` | `src-js/` |
| `src@wasmJs`, `src@wasmWasi` | `src-wasm/` |
| `src@ios`, `src@apple`, `src@native`, `src@linuxX64`, ... | `src-native/` |
| `test` | `tests/` |
| `test@jvm`, `test@android`, ... | `tests-jvm/`, `tests-android/`, ... |
| `resources` | `resources/` |
| `testResources` | `test-resources/` |
| `src/AndroidManifest.xml` | `android/AndroidManifest.xml` |
| `res`, `assets` | `android/res/`, `android/assets/` |

`kargo import` makes these moves with `git mv` when the project is in a git repository. A parent directory that is shared by several targets (`src@native`, `src@apple`, `src@ios`) all goes to `src-native/`; files with the same name collide there, and code for one platform needs `expect`/`actual`. Platform-specific resource directories (`resources@jvm`) have no place in Kargo's layout and are reported.

A module with `layout: maven-like` is read as `src/main/kotlin`, `src/test/kotlin` and so on, and imported like a Maven project.

## Step 3: Turn `project.yaml` into a workspace

`project.yaml` lists modules; Kargo's equivalent is a workspace:

```yaml title="project.yaml"
modules:
  - app
  - libs/core
```

```toml title="kargo.toml"
[workspace]
members = ["app", "libs/core"]
```

A dependency on another module becomes a path dependency:

```yaml title="app/module.yaml"
dependencies:
  - //libs/core
```

```toml title="app/kargo.toml"
[dependencies]
core = { path = "../libs/core" }
```

Kargo's workspace root has no package of its own. If the project has a root `module.yaml` next to the other modules, its code is not imported; move it into a member or keep it as a plain package and move the workspace root up one directory.

## Step 4: Delete the Kotlin Toolchain files

```sh
git rm project.yaml */module.yaml kotlin kotlin.bat amper amper.bat
git rm libs.versions.toml 2>/dev/null # if you have one
echo "/target" >> .gitignore
```

Keep `libs.versions.toml` open while you finish translating dependencies if you used a version catalog; Kargo doesn't read catalogs, so copy the versions into `kargo.toml` (or let `kargo import` do it, which resolves `$libs.*` references).

## Step 5: Build and compare

```sh
kargo build
kargo test
kargo tree      # compare with ./kotlin show dependencies
```

Kargo resolves conflicts like Gradle (highest version wins), so the trees should match. If something differs, `kargo why <group:artifact>` shows who asks for which version and what decided; pin it with `force = true`.

## Reference: Kotlin Toolchain → Kargo

| Kotlin Toolchain | Kargo |
|---|---|
| `product: jvm/app`, `product: jvm/lib` | `[target.jvm]` (a package with no `[target.*]` is JVM) |
| `product: android/app` | `[target.android] kind = "application"` |
| `product: kmp/lib` with `platforms: [jvm, android, iosArm64]` | `[target.jvm]`, `[target.android] kind = "library"`, `[target.native] platforms = ["iosArm64"] kind = "library"` |
| `product: linux/app`, `windows/app`, `macos/app` | `[target.native]` with `platforms` and `kind = "executable"` |
| `product: wasm-js/app` | `[target.wasm] environment = "browser"` |
| `product: wasm-wasi/app` | `[target.wasm] environment = "wasi"` |
| `product: js/app` | `[target.js]` |
| `product: ios/app` | `[target.native] platforms = ["iosArm64", "iosSimulatorArm64"] kind = "framework"`; Swift and the Xcode project stay with Xcode |
| `product: jvm/amper-plugin` | Not supported; write a `kargo-<name>` subcommand |
| `dependencies`, `dependencies@jvm`, `dependencies@ios` | `[dependencies]`, `[target.jvm.dependencies]`, `[target.native.dependencies]` |
| `test-dependencies` | `[dev-dependencies]` (Kargo adds `kotlin-test` and JUnit) |
| `g:a:v`, `g:a:v:classifier`, `g:a:v@aar` | `"g:a" = "v"`, with `classifier` and `extension` in the long form |
| `g:a:v: exported` | `"g:a" = { version = "v", exported = true }` |
| `g:a:v: compile-only` | `scope = "provided"` |
| `g:a:v: runtime-only` | `scope = "runtime"` |
| `bom: g:a:v` | `"g:a" = { version = "v", bom = true }` |
| `$libs.key` | The coordinates and version from `libs.versions.toml` |
| `$compose.*`, `$kotlin.*`, `$ktor.*`, `$springBoot.*`, `$lombok.*` | The matching coordinates, or `[frameworks] compose`, `serialization`, `ktor`, `spring-boot`, `lombok` |
| `//libs/core` | `core = { path = "../libs/core" }` |
| `settings.kotlin.version` | `[kotlin] version` |
| `settings.kotlin.languageVersion`, `apiVersion` | `[kotlin] language-version`, `api-version` |
| `settings.kotlin.optIns`, `freeCompilerArgs` | `[kotlin] opt-in`, `compiler-args` |
| `settings.kotlin.allWarningsAsErrors` | `[kotlin] warnings-as-errors = true` |
| `settings.kotlin.explicitApi` | `compiler-args = ["-Xexplicit-api=strict"]` |
| `settings.kotlin.serialization` | `[frameworks] serialization = true` (with `formats` when it names one) |
| `settings.compose` | `[frameworks] compose = true` |
| `settings.ktor` | `[frameworks] ktor` |
| `settings.springBoot` | `[frameworks] spring-boot` |
| `settings.lombok` | `[frameworks] lombok` |
| `settings.kotlin.allOpen`, `noArg` | `[plugins.allopen]`, `[plugins.noarg]` |
| `settings.kotlin.ksp.version`, `processors` | `[kotlin] ksp` and dependencies with `scope = "ksp"` |
| `settings.java.annotationProcessing.processors` | Dependencies with `scope = "processor"` |
| `settings.jvm.jdk.version` | `[toolchain] jdk` |
| `settings.jvm.release` | `[target.jvm] jvm-target` |
| `settings.jvm.mainClass` | `[target.jvm] main-class` |
| `settings.java.freeCompilerArgs` | `[target.jvm] javac-args` |
| `settings.android.namespace`, `applicationId`, `minSdk`, `targetSdk`, `compileSdk`, `versionCode`, `versionName` | `[target.android] namespace`, `application-id`, `min-sdk`, `target-sdk`, `compile-sdk`, `version-code`, `version-name` |
| `settings.native.entryPoint` | `[target.native] entry-point` |
| `settings.publishing.group`, `version` | `[package] group`, `version`; publish with [`kargo publish`](../guides/publishing/) |
| `repositories` | `[repositories]` (Maven Central and Google Maven are there by default) |
| `./kotlin build` | `kargo build` (`--release` for an optimized build) |
| `./kotlin test` | `kargo test` |
| `./kotlin run -m app` | `kargo run -p app` |
| `./kotlin publish` | `kargo publish` |
| `./kotlin show dependencies` | `kargo tree` |
| `./kotlin show modules` | `kargo metadata` |
| `./kotlin new` | `kargo new` |
| `./kotlin clean` | `kargo clean` |
| `./kotlin update` | `kargo self update` |

## What does not translate

`kargo import` reads the YAML statically. These have no Kargo equivalent, and the report lists each one it finds:

- **`apply` templates and `project.yaml` plugins.** Kargo has no build plugin system. Copy what a template sets into `kargo.toml`, and recreate a plugin as a `kargo-<name>` subcommand or a `[tasks]`/hook entry.
- **`mavenPlugins` and `pluginInfo`.** Kargo runs the Kotlin compiler directly; the JDK's `javac` handles Java annotation processing.
- **`settings.publishing` beyond `group` and `version`.** Signing, sources jars and the Central Portal are configured for [`kargo publish`](../guides/publishing/), which signs with GnuPG.
- **`settings.kotlin.powerAssert`, `dataframe`, `rpc`, `jsPlainObjects` and third-party `compilerPlugins` whose Maven coordinate is not a Kotlin compiler plugin.** Third-party compiler plugins with a `dependency` (Maven coordinates) and an `id` do translate to `[plugins.<name>]`.
- **SwiftPM dependencies, CocoaPods and `swiftPackage`.** Kotlin/Native's C interop is configured with [`[target.native.cinterop]`](../guides/c-interop/) instead.
- **Platform-specific `settings@...` and `resources@...`.** Kargo has no per-target settings table or resource directory; the importer applies platform-specific settings to the whole package and reports platform-specific resources.
- **A source directory shared by several targets** (`src@web` in a project with both JS and Wasm, or an alias such as `jvmAndAndroid`). Kargo's fixed layout has no place for it, so it is reported instead of moved.
- **`settings.junit = "junit-4"` and `test-settings`.** `kargo test` runs JUnit 6; JUnit 5 tests run unchanged, JUnit 4 tests need the vintage engine.

After an import, run `kargo tree --duplicates` to compare the resolved versions with the old build, and read [Dependencies](../guides/dependencies/) for Kargo's resolution rules.
