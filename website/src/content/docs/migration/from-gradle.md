---
title: Migrating from Gradle
description: Translate build.gradle.kts into kargo.toml, step by step.
---

This guide moves a Gradle project, JVM or Kotlin Multiplatform, to Kargo. Most single-module projects take about fifteen minutes. To see first what Gradle features Kargo does not have, read [Gradle feature parity](../gradle-feature-parity/).

:::tip
`kargo import` writes most of this `kargo.toml` for you and moves the sources: it reads the plugins, dependencies, version catalogs, targets and source sets of your build scripts and lists what it could not translate. See [Importing a project](../../guides/importing/), then use this guide for the parts the report points at.
:::

:::caution[Before you start]
Kargo builds **one package per `kargo.toml`**. In a multi-module build, convert each module with the steps below, list the modules as `members` of a [workspace](../../guides/workspaces/) (the equivalent of `settings.gradle.kts`), and turn `implementation(project(":core"))` into `core = { path = "../core" }`. An Android library module (`com.android.library`) becomes a package with `[target.android] kind = "library"`, which Android apps and libraries use the same way. Gradle plugins that generate code (protobuf, OpenAPI generator) become [`[[generate]]` steps](../../guides/code-generation/); other plugins with no Kargo equivalent can usually be replaced with a [task on a `pre-build` hook](../../guides/tasks-and-plugins/).
:::

## Step 1: Create `kargo.toml`

Start from your `build.gradle.kts`:

```kotlin title="build.gradle.kts"
plugins {
    kotlin("jvm") version "2.4.21"
    kotlin("plugin.serialization") version "2.4.21"
    application
}

group = "com.example"
version = "1.2.0"

repositories {
    mavenCentral()
    maven("https://jitpack.io")
}

dependencies {
    implementation(platform("io.ktor:ktor-bom:3.6.0"))
    implementation("io.ktor:ktor-client-cio")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.11.0")
    implementation("com.github.ajalt.clikt:clikt:5.0.3")
    runtimeOnly("ch.qos.logback:logback-classic:1.5.32")
    compileOnly("org.jetbrains:annotations:26.0.2")
    testImplementation(kotlin("test"))
    testImplementation("io.mockk:mockk:1.14.5")
}

kotlin {
    jvmToolchain(21)
    compilerOptions {
        freeCompilerArgs.add("-Xcontext-parameters")
        optIn.add("kotlin.ExperimentalStdlibApi")
    }
}

application {
    mainClass.set("com.example.cli.MainKt")
    applicationDefaultJvmArgs = listOf("-Xmx512m")
}
```

The Kargo equivalent:

```toml title="kargo.toml"
[package]
name = "cli"
version = "1.2.0"
group = "com.example"

[kotlin]
version = "2.4.21"
compiler-args = ["-Xcontext-parameters"]
opt-in = ["kotlin.ExperimentalStdlibApi"]

[toolchain]
jdk = "21"

[target.jvm]
jvm-target = "21"
main-class = "com.example.cli.MainKt"     # optional: Kargo finds `fun main` by itself
jvm-args = ["-Xmx512m"]

[frameworks]
serialization = true                      # plugin + kotlinx-serialization-json

[dependencies]
"io.ktor:ktor-bom" = { version = "3.6.0", bom = true }
"io.ktor:ktor-client-cio" = {}
"com.github.ajalt.clikt:clikt" = "5.0.3"
"ch.qos.logback:logback-classic" = { version = "1.5.32", scope = "runtime" }
"org.jetbrains:annotations" = { version = "26.0.2", scope = "provided" }

[dev-dependencies]
"io.mockk:mockk" = "1.14.5"               # kotlin("test") is always included

[repositories]
jitpack = "https://jitpack.io"            # mavenCentral() is always included
```

## Step 2: Move the sources

Kargo uses flat directories:

| Gradle | Kargo |
|---|---|
| `src/main/kotlin`, `src/main/java` | `src/` |
| `src/main/resources` | `resources/` |
| `src/test/kotlin`, `src/test/java` | `tests/` |
| `src/test/resources` | `test-resources/` |
| `src/benchmark/kotlin`, `src/jmh/kotlin` | `benches/` |

```sh
git mv src/main/kotlin src-tmp && git mv src/test/kotlin tests
git mv src/main/resources resources 2>/dev/null; git mv src/test/resources test-resources 2>/dev/null
rm -rf src && git mv src-tmp src
# Java sources, if any:  git mv src/main/java/* src/
```

For Kotlin Multiplatform:

| Gradle source set | Kargo |
|---|---|
| `src/commonMain/kotlin` | `src/` |
| `src/jvmMain/kotlin` | `src-jvm/` |
| `src/wasmJsMain/kotlin` | `src-wasm/` |
| `src/jsMain/kotlin` | `src-js/` |
| `src/nativeMain/kotlin`, `src/linuxX64Main/kotlin`, ... | `src-native/` |
| `src/androidMain/kotlin` | `src-android/` |
| `src/commonTest/kotlin` | `tests/` |
| `src/jvmTest/kotlin` | `tests-jvm/` |
| `src/jvmAndAndroidMain/kotlin` (a source set of the hierarchy template's `dependsOn` graph) | `src-jvmAndAndroid/` with `[source-sets.jvmAndAndroid] targets = ["jvm", "android"]` |
| `src/webMain/kotlin`, `src/nonJsMain/kotlin`, ... | `src-web/`, `src-nonJs/`, each with its `[source-sets.<name>]` |
| `src/appleMain/kotlin`, `src/iosMain/kotlin`, `src/linuxMain/kotlin` | `src-apple/`, `src-ios/`, `src-linux/`, each with `[source-sets.<name>] platforms = [...]` |

`appleMain`, `linuxMain` and the other source sets of Native platforms become `[source-sets.<name>]` with `platforms` (`platforms = ["apple"]`); `src-native/` is `nativeMain`, which they sit below. See [Source sets for several targets](../../guides/multiplatform/#source-sets-for-several-targets) and [Sets of some Native platforms](../../guides/multiplatform/#sets-of-some-native-platforms).

## Step 3: Delete Gradle

```sh
git rm -r build.gradle.kts settings.gradle.kts gradle.properties gradlew gradlew.bat gradle/
echo "/target" >> .gitignore
```

Keep `gradle/libs.versions.toml` open while you finish step 1 if you used a version catalog; Kargo doesn't read catalogs, so copy the versions into `kargo.toml`.

## Step 4: Build and compare

```sh
kargo build
kargo test
kargo tree      # compare with ./gradlew dependencies --configuration runtimeClasspath
```

Kargo resolves conflicts like Gradle (highest version wins), so the trees should match. If something differs, `kargo why <group:artifact>` shows who asks for which version and what decided, as `dependencyInsight` does; pin it with `force = true`.

## Reference: Gradle → Kargo

| Gradle | Kargo |
|---|---|
| `kotlin("jvm") version "X"` | `[kotlin] version = "X"` and `[target.jvm]` |
| `kotlin("multiplatform")` with `jvm()`, `wasmJs()`, `linuxX64()` | `[target.jvm]`, `[target.wasm]`, `[target.native] platforms = ["linux_x64"]` |
| `compilations["main"].cinterops { create("sqlite") { ... } }` | `[target.native.cinterop.sqlite]`, with the `.def` file in `interop/sqlite.def` (or `headers` and the other `.def` keys in `kargo.toml`) |
| `linkerOpts.linux = ...` in a `.def` file, or per-target `cinterops` blocks | `[target.native.cinterop.sqlite.platform.linux_x64]` |
| `binaries.framework { baseName = "Shared"; export(...) }` | `[target.native] kind = "framework"`, `export = [...]` |
| `XCFramework()` + `assembleXCFramework` | `kargo build --xcframework` |
| A CMake or `exec` task that builds bundled C code | `[target.native.c-sources]` |
| `wasmWasi()` | `[target.wasm] environment = "wasi"` |
| `js { nodejs() }`, `js { browser() }` (`js(IR)`) | `[target.js] environment = "node"` or `"browser"`; see [JavaScript](../../guides/js/) |
| `useEsModules()`, `useCommonJs()`, the default UMD | `[target.js] module-kind = "es"` (Kargo's default), `"commonjs"`, `"umd"` |
| `binaries.library()` with `generateTypeScriptDefinitions()` | `[target.js] kind = "library"` (declarations included) |
| `jsMain.dependencies { implementation(npm("x", "1.0")) }` | `[target.js.npm-dependencies] x = "1.0"` |
| `com.android.application` + `android { ... }` | `[target.android]` |
| `signingConfigs { create("release") { ... } }` | `[target.android.signing.release]`, with passwords in `store-password-env` / `key-password-env` |
| `buildTypes { release { isMinifyEnabled = true; proguardFiles(...) } }` | `[target.android] minify = true`, `proguard-files = [...]` |
| `buildConfigField("String", "API_URL", "\"...\"")` | `[target.android] build-config = { API_URL = "..." }` |
| `flavorDimensions` + `productFlavors { create("free") { ... } }` | `flavor-dimensions` and `[target.android.flavors.free]` |
| `./gradlew bundleRelease` | `kargo package -t android --release` |
| `implementation(...)` | `[dependencies]` |
| `api(...)` | `exported = true` in `[dependencies]` |
| `include(":core")` in `settings.gradle.kts` | `[workspace] members = ["core"]` (or a glob such as `"libs/*"`) |
| `implementation(project(":core"))` | `core = { path = "../core" }` |
| `api(project(":core"))` | `core = { path = "../core", exported = true }` |
| `java-test-fixtures` + `testImplementation(testFixtures(project(":core")))` | `test-fixtures/` and `core = { path = "../core", test-fixtures = true }` in `[dev-dependencies]` |
| `subprojects { ... }` / convention plugins for shared settings | `[workspace.package]`, `[workspace.kotlin]`, `[workspace.dependencies]`, `[workspace.frameworks]` |
| `./gradlew :server:run` | `kargo run -p server` |
| `runtimeOnly(...)` | `scope = "runtime"` |
| `compileOnly(...)` | `scope = "provided"` |
| `annotationProcessor(...)` | `scope = "processor"` |
| `options.compilerArgs` (`JavaCompile`) | `[target.jvm] javac-args` |
| `ksp(...)` (the `com.google.devtools.ksp` plugin) | `scope = "ksp"` |
| `kspTest(...)` | `scope = "ksp"` in `[dev-dependencies]` |
| `kapt(...)` | not supported; use the library's KSP processor (`scope = "ksp"`) |
| `testImplementation(...)` | `[dev-dependencies]` |
| `jvmMainImplementation(...)` / `sourceSets.jvmMain.dependencies` | `[target.jvm.dependencies]` |
| `platform(...)` / `enforcedPlatform(...)` | `bom = true` |
| `exclude(group = "g", module = "m")` | `exclude = ["g:m"]` |
| `isTransitive = false` | `transitive = false` |
| `strictly(...)` / `force` | `force = true` |
| `kotlin("plugin.serialization")` | `[frameworks] serialization = true` |
| `org.jetbrains.kotlinx.benchmark` with `benchmark { targets { register("main") } }`, `me.champeau.jmh`, then `./gradlew benchmark` or `jmh` | `[frameworks] benchmark = true` and `kargo bench`; the configuration's `iterations`, `warmups`, `iterationTime`, `mode` and `outputTimeUnit` are `[bench] iterations`, `warmup`, `mode` and `time-unit` (see [Benchmarking](../../guides/benchmarking/)) |
| `kotlin("plugin.spring")`, `org.springframework.boot` | `[frameworks] spring-boot = true` |
| `kotlin("plugin.jpa")` | `spring-boot = { starters = ["data-jpa"] }` or `jpa = true` |
| `org.jetbrains.compose` + `kotlin("plugin.compose")` | `[frameworks] compose = true` |
| `io.ktor.plugin` | `[frameworks] ktor = { server = "netty" }` |
| `io.freefair.lombok` + `kotlin("plugin.lombok")` | `[frameworks] lombok = true` |
| Other compiler plugins | `[plugins.<name>] maven = "...", id = "..."` |
| `jvmToolchain(21)` | `[toolchain] jdk = "21"` |
| `org.beryx.jlink` + `./gradlew jlink` | `kargo package --format jlink`, with `[package.jlink] modules` for extra modules |
| ProGuard or R8 plugin for a JVM jar | `[package.optimize]` and `kargo package --release` (or `kargo package --optimize`), with `proguard-rules.pro` for keep rules |
| `org.graalvm.buildtools.native` + `./gradlew nativeCompile` | `kargo package --format native-image`, with `[toolchain] graalvm` and `[package.native-image]` |
| `-Pagent` / `nativeCompile` with the tracing agent | `kargo run --native-agent` |
| `org.beryx.jlink`'s `jpackage` task | `kargo package --format installer`, with `[package.installer]` |
| Compose's `compose.desktop { application { nativeDistributions { ... } } }` and `packageDistributionForCurrentOS` | `kargo package --desktop`, with `[package.installer]`: `targetFormats` is `type`, `packageName` is `name`, `windows.iconFile` and friends are `icon`, `fileAssociation(...)` is `file-associations`, `macOS.signing` is `signing-identity` and `macOS.notarization` is `notarize` |
| Compose's `packageUberJarForCurrentOS` | `kargo package --desktop --format jar` (`--platforms all` for every OS) |
| `compose.resources { packageOfResClass = ...; publicResClass = true }` and `src/commonMain/composeResources/` | `[frameworks] compose = { resources-package = "...", public-resources = true }` and `composeResources/` |
| `jvmTarget = JvmTarget.JVM_17` | `[target.jvm] jvm-target = "17"` |
| `freeCompilerArgs` | `[kotlin] compiler-args` |
| `optIn` | `[kotlin] opt-in` |
| `allWarningsAsErrors = true` | `[kotlin] warnings-as-errors = true` |
| `application { mainClass }` | `[target.jvm] main-class` |
| `distZip`, `distTar`, `installDist` (the `application` plugin's distribution) | `kargo package --dist`: `bin/<name>` and `bin/<name>.bat` start scripts and `lib/` in a zip and a tar.gz, with `applicationDefaultJvmArgs` as `jvm-args` and `from(...)` as `[package.dist] include`; see [Distribution archives](../../guides/distributing/#distribution-archives) |
| `shadowJar` | `kargo package` (`--fat` for a library); `relocate`, `exclude`, `minimize()` and `mergeServiceFiles()` are [`[package.shade]`](../guides/distributing/#shading) |
| `maven-publish` + `signing` (or `com.vanniktech.maven.publish`) | `kargo publish`, with `[publish]` and the POM fields in `[package]`; see [Publishing a library](../../guides/publishing/) |
| `./gradlew publishToMavenLocal` | `kargo publish --local` |
| `mavenLocal()` | `[repositories] local = "maven-local"` |
| `exclusiveContent { forRepository { maven(url) }; filter { includeGroup("com.corp") } }` | `[repositories.corp] url = "..."`, `include-groups = ["com.corp"]` |
| `content { excludeGroup("com.corp") }` | `exclude-groups = ["com.corp"]` on the repository (`[repositories.maven-central]` for `mavenCentral()`) |
| `tasks.register("x") { exec ... }` | `[tasks] x = "..."`, or `x = { script = "scripts/x.kts" }` for a task written in Kotlin |
| `tasks.compileKotlin { dependsOn(gen) }` | `gen = { script = "scripts/gen.kts", hooks = ["pre-build"] }` (or `run = "..."` for a shell command) |
| `com.google.protobuf` plugin, `sourceSets { main { kotlin.srcDir("build/generated/...") } }` | a [`[[generate]]` step](../../guides/code-generation/) such as `builtin = "protobuf"`; its output directory is a source root |
| `./gradlew run --args="a b"` | `kargo run -- a b` |
| extra `JavaExec` tasks for other mains | `[[bin]]` and `kargo run --bin <name>`, or `kargo run --main <class>` |
| `./gradlew test --tests "*Foo*"` | `kargo test Foo` |
| `testing { suites { integration { ... } } }` (`jvm-test-suite`) | `[test.suites.integration]` with `sources = "integration-tests"`, run with `kargo test --suite integration` (see [Test suites](../../guides/testing/#test-suites)) |
| `./gradlew dependencies` | `kargo tree` |
| `./gradlew dependencyInsight --dependency <module>` | `kargo why <group:artifact>` (or `kargo tree --invert <group:artifact>`) |
| `./gradlew dependencyUpdates` (Versions plugin) | `kargo outdated`, then `kargo upgrade` |
| `./gradlew dependencies --update-locks g:a` | `kargo update g:a` |
| `./gradlew dokkaGenerate` (Dokka plugin) | `kargo doc` |
| `./gradlew ktlintFormat` / `ktlintCheck` (ktlint plugins, Spotless) | `kargo fmt` / `kargo fmt --check` |
| `./gradlew detekt` (detekt plugin) | `kargo lint`; keep your config with `[lint] config = ["config/detekt/detekt.yml"]` |
| `--offline` | `--offline` |
| `kotlin.incremental=true` (incremental compilation) | On by default for the JVM; `KARGO_INCREMENTAL=false` turns it off |
| Gradle daemon / Kotlin daemon (`--no-daemon`, `kotlin.compiler.execution.strategy`) | The [compiler daemon](../../guides/build-performance/#the-compiler-daemon) (`KARGO_DAEMON=false`, `kargo daemon stop`) |
| `org.gradle.parallel=true` | Always on: targets, Native platforms and independent workspace members build in parallel (`-j` limits members) |
| `gradle.lockfile` | `kargo.lock` (always on) |
