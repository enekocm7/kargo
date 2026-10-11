---
title: Migrating from Gradle
description: Translate build.gradle.kts into kargo.toml, step by step.
---

This guide moves a single-module Gradle project, JVM or Kotlin Multiplatform, to Kargo. Most projects take about fifteen minutes.

:::caution[Before you start]
Kargo builds **one package per `kargo.toml`**. Multi-module Gradle builds can't be converted in one go: each module becomes its own Kargo project. Gradle plugins with no Kargo equivalent (for example, code generators) can usually be replaced with a [task on a `pre-build` hook](../../guides/tasks-and-plugins/).
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
| `src/nativeMain/kotlin`, `src/linuxX64Main/kotlin`, ... | `src-native/` |
| `src/androidMain/kotlin` | `src-android/` |
| `src/commonTest/kotlin` | `tests/` |
| `src/jvmTest/kotlin` | `tests-jvm/` |

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

Kargo resolves conflicts like Gradle (highest version wins), so the trees should match. If something differs, pin it with `force = true`.

## Reference: Gradle → Kargo

| Gradle | Kargo |
|---|---|
| `kotlin("jvm") version "X"` | `[kotlin] version = "X"` and `[target.jvm]` |
| `kotlin("multiplatform")` with `jvm()`, `wasmJs()`, `linuxX64()` | `[target.jvm]`, `[target.wasm]`, `[target.native] platforms = ["linux_x64"]` |
| `wasmWasi()` | `[target.wasm] environment = "wasi"` |
| `com.android.application` + `android { ... }` | `[target.android]` |
| `implementation(...)` | `[dependencies]` |
| `api(...)` | `[dependencies]` |
| `runtimeOnly(...)` | `scope = "runtime"` |
| `compileOnly(...)` | `scope = "provided"` |
| `testImplementation(...)` | `[dev-dependencies]` |
| `jvmMainImplementation(...)` / `sourceSets.jvmMain.dependencies` | `[target.jvm.dependencies]` |
| `platform(...)` / `enforcedPlatform(...)` | `bom = true` |
| `exclude(group = "g", module = "m")` | `exclude = ["g:m"]` |
| `isTransitive = false` | `transitive = false` |
| `strictly(...)` / `force` | `force = true` |
| `kotlin("plugin.serialization")` | `[frameworks] serialization = true` |
| `kotlin("plugin.spring")`, `org.springframework.boot` | `[frameworks] spring-boot = true` |
| `kotlin("plugin.jpa")` | `spring-boot = { starters = ["data-jpa"] }` or `jpa = true` |
| `org.jetbrains.compose` + `kotlin("plugin.compose")` | `[frameworks] compose = true` |
| `io.ktor.plugin` | `[frameworks] ktor = { server = "netty" }` |
| `io.freefair.lombok` + `kotlin("plugin.lombok")` | `[frameworks] lombok = true` |
| Other compiler plugins | `[plugins.<name>] maven = "...", id = "..."` |
| `jvmToolchain(21)` | `[toolchain] jdk = "21"` |
| `jvmTarget = JvmTarget.JVM_17` | `[target.jvm] jvm-target = "17"` |
| `freeCompilerArgs` | `[kotlin] compiler-args` |
| `optIn` | `[kotlin] opt-in` |
| `allWarningsAsErrors = true` | `[kotlin] warnings-as-errors = true` |
| `application { mainClass }` | `[target.jvm] main-class` |
| `shadowJar` | `kargo package` |
| `tasks.register("x") { exec ... }` | `[tasks] x = "..."` |
| `tasks.compileKotlin { dependsOn(gen) }` | `gen = { run = "...", hooks = ["pre-build"] }` |
| `./gradlew run --args="a b"` | `kargo run -- a b` |
| `./gradlew test --tests "*Foo*"` | `kargo test Foo` |
| `./gradlew dependencies` | `kargo tree` |
| `--offline` | `--offline` |
| `gradle.lockfile` | `kargo.lock` (always on) |
