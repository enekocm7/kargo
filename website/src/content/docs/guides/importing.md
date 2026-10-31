---
title: Importing a Maven, Gradle or Kotlin Toolchain project
description: Convert an existing Maven, Gradle or Kotlin Toolchain project into a Kargo project with kargo import.
---

`kargo import` reads the `pom.xml`, the `build.gradle(.kts)` files or the `project.yaml` and `module.yaml` files of a project and writes the `kargo.toml` for it. It also moves the sources into [Kargo's layout](../../getting-started/project-layout/) and builds the result to check that it compiles. Your old build files stay where they are, so you can keep building with Maven, Gradle or the Kotlin Toolchain while you try Kargo.

```sh
cd my-project
kargo import --dry-run     # print the kargo.toml and the file moves, change nothing
kargo import               # do it, then run kargo build
```

```console
$ kargo import
     Created kargo.toml
       Moved 3 source files into Kargo's layout with git mv
    Imported Gradle project as package `ktor-sample`
  Translated ktor-sample: Ktor 3.0.0 -> [frameworks] ktor (server: netty, client: none)
  Translated ktor-sample: kotlinx.serialization (core) -> [frameworks] serialization
  Translated ktor-sample: 1 dependencies, Kotlin 2.4.21
   Compiling ktor-sample v0.0.1 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.66s
```

| Option | Meaning |
|---|---|
| `[path]` | The project directory (default: the current one) |
| `--dry-run` | Print every `kargo.toml` and every file move, and change nothing. The report of what was translated and what was not is printed too |
| `--no-build` | Don't run `kargo build` at the end |
| `--from <maven\|gradle\|kotlin>` | Which build tool to read, when the directory has the files of several (`amper` works for `kotlin`) |

`kargo import` stops without changing anything when the directory already has a `kargo.toml`, when it has no build files, or when two files would land on the same path in the new layout. It plans every move before it makes the first one. In a git repository the moves use `git mv`, so they show up as renames, and `/target` is added to `.gitignore` when the file doesn't ignore it yet.

If the build at the end fails, the project is imported all the same: the error says so, `kargo import` exits with code 101, and you fix what `kargo build` reports.

## What it reads

### Maven

A `pom.xml` is static, so it is translated exactly, with the parent POMs and BOMs resolved the way a Kargo build resolves them. A parent that is a file next to the project (`<relativePath>`, `<modules>`) is read from disk, any other from Maven Central or the project's `<repositories>`.

| In `pom.xml` | In `kargo.toml` |
|---|---|
| `groupId`, `artifactId`, `version`, `description`, `url`, `licenses`, `developers`, `scm` | `[package]` |
| `<properties>`, inherited ones too | Filled in wherever `${...}` was used |
| `<dependencies>` and their scopes, `<exclusions>`, `<classifier>`, `<type>` | `[dependencies]` and `[dev-dependencies]` (see the table in [Migrating from Maven](../../migration/from-maven/)) |
| Versions from `<dependencyManagement>` and from the parent | The version, written out; a BOM the project imports itself stays `bom = true` and the dependencies it manages keep no version |
| `kotlin-maven-plugin`: version, `languageVersion`, `apiVersion`, `<args>`, `jvmTarget` | `[kotlin]` and `[target.jvm]`; `-opt-in=` becomes `opt-in`, `-Werror` `warnings-as-errors` |
| `<compilerPlugins>`: `spring`, `jpa`, `all-open`, `no-arg`, `kotlinx-serialization`, `lombok` | `[frameworks]` or `[plugins.<name>]` |
| `maven-compiler-plugin`: `release`, `target`, `compilerArgs`, `annotationProcessorPaths` | `jvm-target`, `javac-args`, `scope = "processor"` |
| `maven-jar-plugin`, `exec-maven-plugin`, `spring-boot-maven-plugin` main class, `start-class` | `[target.jvm] main-class` |
| `<repositories>` | `[repositories]` |
| `<modules>` | A [workspace](../workspaces/); a dependency on another module becomes a `path` dependency |

In a multi-module build, the compile-scope dependencies of a module that other modules use get `exported = true`, which is what Maven's transitive compile scope means.

### Gradle

Gradle builds are programs, so `kargo import` reads their declarative parts and lists what it could not understand. It reads Kotlin and Groovy scripts, `settings.gradle(.kts)`, `gradle.properties`, `val`s and `extra` properties that hold strings, and version catalogs (`gradle/libs.versions.toml`, with `libs.versions.x`, `libs.x.y`, `libs.bundles.x` and `libs.plugins.x`). It does not run Gradle.

| In the Gradle build | In `kargo.toml` |
|---|---|
| `kotlin("jvm")` | A JVM project |
| `kotlin("multiplatform")` with `jvm()`, `wasmJs`, `wasmWasi`, `js` and the Native presets (`linuxX64()`, `macosArm64()`, ...) | `[target.jvm]`, `[target.wasm]`, `[target.js]` and `[target.native]` with `platforms`. A target with `binaries.executable()` is an application, one without it a library. `js { browser() }` sets `environment = "browser"`, and the JS module kind follows `useEsModules()` and `useCommonJs()` (`umd`, Gradle's default, otherwise) |
| `com.android.application`, `com.android.library` with `android { ... }` | `[target.android]`: `namespace`, `application-id`, the SDK versions, `version-code`, `version-name`, `minify` |
| `implementation`, `api`, `compileOnly`, `runtimeOnly`, `testImplementation`, `ksp`, `annotationProcessor` | `[dependencies]`, `exported = true` for `api`, `scope`, `[dev-dependencies]` |
| `platform("g:a:v")` | `bom = true` |
| `project(":core")` | `core = { path = "../core" }`, and `include(...)` of `settings.gradle` makes the projects a workspace |
| `kotlin("test")`, `kotlin("stdlib")`, JUnit | Left out: Kargo adds them |
| Dependencies of source sets (`commonMain.dependencies { }`, `val jvmMain by getting { dependencies { } }`) | `[dependencies]`, `[target.<target>.dependencies]`; `npm("name", "version")` of Wasm becomes `[target.wasm.npm-dependencies]`, and of JS `[target.js.npm-dependencies]`. A dependency of a test source set goes to `[dev-dependencies]` for every target |
| `org.springframework.boot`, `io.ktor.plugin`, `org.jetbrains.compose`, `kotlin("plugin.serialization")`, `plugin.spring`, `plugin.jpa`, `io.freefair.lombok` and the matching libraries | `[frameworks]` |
| `kotlinOptions` / `compilerOptions`: `jvmTarget`, `freeCompilerArgs`, `allWarningsAsErrors`, `optIn`, `languageVersion`, `apiVersion` | `[kotlin]` and `[target.jvm] jvm-target` |
| `jvmToolchain(21)`, `java.toolchain.languageVersion` | `jvm-target` and `[toolchain] jdk` |
| `application { mainClass.set(...) }`, `mainClassName` | `[target.jvm] main-class` |
| `allOpen { annotation(...) }`, `noArg { annotation(...) }` | `[plugins.allopen]`, `[plugins.noarg]` |
| `repositories { maven("url") }`, `mavenLocal()` | `[repositories]` (Maven Central and Google Maven are there by default) |
| `group`, `version`, `description`, `rootProject.name` | `[package]` |

A multi-project build keeps what `allprojects { }`, `subprojects { }` and `project(":x") { }` of the root script configure, and plugin versions the root script declares with `apply false`.

### Kotlin Toolchain

The [Kotlin Toolchain](https://kotlin-toolchain.org/) (formerly Amper) is configured in YAML. `kargo import` reads `project.yaml`, every `module.yaml` it lists, the `src@<platform>` directories and the project catalog (`libs.versions.toml` at the root or in `gradle/`). It does not run the toolchain or follow `apply` templates.

| In the Kotlin Toolchain | In `kargo.toml` |
|---|---|
| `product: jvm/app`, `jvm/lib` | `[target.jvm]` (or no `[target.*]` at all) |
| `product: android/app` | `[target.android] kind = "application"` |
| `product: kmp/lib` with `platforms` | `[target.jvm]`, `[target.android]`, `[target.wasm]`, `[target.js]` and `[target.native] platforms = [...]`, with `kind = "library"` where the toolchain does |
| `product: linux/app`, `macos/app`, `windows/app` | `[target.native] platforms = [...] kind = "executable"` |
| `product: wasm-js/app`, `wasm-wasi/app`, `js/app` | `[target.wasm] environment = "browser"` / `"wasi"`, `[target.js]` |
| `product: ios/app` | `[target.native] platforms = ["iosArm64", "iosSimulatorArm64"] kind = "framework"`; the Swift part needs Xcode |
| `dependencies`, `dependencies@jvm`, `dependencies@ios` | `[dependencies]`, `[target.jvm.dependencies]`, `[target.native.dependencies]` |
| `test-dependencies` | `[dev-dependencies]` |
| `exported`, `compile-only`, `runtime-only` | `exported = true`, `scope = "provided"`, `scope = "runtime"` |
| `bom: g:a:v` | `bom = true` |
| `//libs/core` | `core = { path = "../libs/core" }` |
| `$libs.key` | The coordinates and version from `libs.versions.toml` |
| `$compose.*`, `$kotlin.*`, `$ktor.*`, `$springBoot.*` | The coordinates, or the matching `[frameworks]` entry |
| `settings.kotlin.version`, `languageVersion`, `apiVersion`, `optIns`, `freeCompilerArgs`, `allWarningsAsErrors` | `[kotlin]` |
| `settings.kotlin.serialization`, `settings.compose` | `[frameworks] serialization`, `compose` |
| `settings.ktor`, `springBoot`, `lombok` | `[frameworks] ktor`, `spring-boot`, `lombok` |
| `settings.kotlin.allOpen`, `noArg`, `ksp` | `[plugins.allopen]`, `[plugins.noarg]`, `[kotlin] ksp` |
| `settings.jvm.jdk.version`, `release`, `mainClass` | `[toolchain] jdk`, `[target.jvm] jvm-target`, `main-class` |
| `settings.java.annotationProcessing.processors` | Dependencies with `scope = "processor"` |
| `settings.android.namespace`, `applicationId`, `minSdk`, `targetSdk`, `compileSdk`, `versionCode`, `versionName` | `[target.android]` |
| `settings.native.entryPoint` | `[target.native] entry-point` |
| `repositories` | `[repositories]` |
| `project.yaml` `modules` | A [workspace](../workspaces/); a dependency between modules becomes a `path` dependency |

`settings.kotlin.serialization` and `settings.compose` become frameworks, so the libraries they add are not repeated in `[dependencies]`. `settings.publishing`, `mavenPlugins`, `apply` templates, SwiftPM dependencies, `settings@...` (platform-specific settings) and platform-specific resources have no Kargo equivalent; the report lists them. See [Migrating from the Kotlin Toolchain](../../migration/from-kotlin-toolchain/) for the differences in how the two tools work.

## Where the sources go

| Maven, Gradle and the Kotlin Toolchain | Kargo |
|---|---|
| `src/main/kotlin`, `src/main/java` | `src/` |
| `src/test/kotlin`, `src/test/java` | `tests/` |
| `src/main/resources` | `resources/` |
| `src/test/resources` | `test-resources/` |
| `src`, `test`, `resources`, `testResources` (Kotlin Toolchain) | `src/`, `tests/`, `resources/`, `test-resources/` |
| `src@jvm`, `test@jvm` | `src-jvm/`, `tests-jvm/` |
| `src@android`, `test@android` | `src-android/`, `tests-android/` |
| `src@js`, `test@js` | `src-js/`, `tests-js/` |
| `src@wasmJs`, `src@wasmWasi` | `src-wasm/`, `tests-wasm/` |
| `src@native`, `src@apple`, `src@ios`, `src@linuxX64`, ... | `src-native/`, `tests-native/` |
| `src/commonMain`, `src/commonTest` | `src/`, `tests/` |
| `src/jvmMain`, `src/jvmTest` | `src-jvm/`, `tests-jvm/` |
| `src/wasmJsMain`, `src/wasmJsTest` | `src-wasm/`, `tests-wasm/` |
| `src/jsMain`, `src/jsTest` | `src-js/`, `tests-js/` |
| `src/nativeMain`, `src/linuxX64Main`, `src/iosMain`, ... | `src-native/`, `tests-native/` |
| `src/androidMain`, or `src/main` of an Android module | `src-android/` (`src/` for a module with only the Android target), `android/AndroidManifest.xml`, `android/res/`, `android/assets/` |
| `src/AndroidManifest.xml`, `res`, `assets` (Kotlin Toolchain) | `android/AndroidManifest.xml`, `android/res/`, `android/assets/` |
| `<sourceDirectory>`, `sourceSets { main { kotlin.srcDir(...) } }` | `src/` |

Files from `src/main/kotlin` and `src/main/java` merge into one tree. The directories the moves leave empty are removed. The Kotlin/Native source sets of several platforms all go to `src-native/`; the report says so, because files with the same name collide there and code for one platform needs `expect`/`actual`.

## The report

After the import, `kargo import` lists what it translated and, with a link to the page that helps, what needs your attention:

- Custom tasks, listed by name; recreate them as [`[tasks]` or hooks](../tasks-and-plugins/).
- Plugins with no setting in Kargo. For the well-known ones the report names the command that replaces them: `maven-publish` (`kargo publish`), Shadow (`kargo package`), ktlint and Spotless (`kargo fmt`), detekt (`kargo lint`), Dokka (`kargo doc`).
- kapt, which Kargo does not run. Use a [KSP processor](../dependencies/#kotlin-symbol-processing-ksp) or a Java annotation processor.
- Dependencies written as `files(...)`, with a variable Kargo could not read, or from a catalog entry that doesn't exist.
- Source sets and Android variants (`debug`, `release`, product flavors, instrumented tests) that have no place in Kargo's layout.
- A parent POM that could not be read. Make it reachable and import again.
- Projects that apply the standalone Kotlin/JS plugin (`kotlin("js")`), and other things Kargo does not build or translate.
- Kotlin Toolchain keys with no equivalent: `apply` templates, `mavenPlugins`, `settings.publishing` beyond the group and version, SwiftPM dependencies, platform-specific settings and resources, and source directories shared by several targets.

Credentials of private repositories are not copied. Name the environment variables that hold them in `username-env` and `password-env` of the [`[repositories]`](../../reference/manifest/#repositories) entry.

## Limits

- Gradle is read, not run. A build that computes its dependencies or settings in code, or hides them in convention plugins (`buildSrc`, `build-logic`), imports only the part that is declared in the scripts, and the report lists the rest.
- The sources always move into Kargo's layout; there is no way to leave them where they are, because Kargo's layout is fixed.
- A Gradle project whose root also includes other projects imports those projects as the members of a workspace; the code of the root project itself is not imported.
- Android modules import their target, the manifest and the resources, but flavors, build types besides `release` minification and signing configurations are listed for you to translate.

After an import, run `kargo tree --duplicates` to compare the resolved versions with the old build (Kargo picks the highest version, like Gradle, where Maven picks the nearest), and read [Migrating from Gradle](../../migration/from-gradle/), [Migrating from Maven](../../migration/from-maven/) or [Migrating from the Kotlin Toolchain](../../migration/from-kotlin-toolchain/) for the differences in how the two tools work.
