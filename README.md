# Kargo

**Kargo is Cargo for Kotlin**: one `kargo.toml`, one CLI, to create, build, run and test Kotlin projects for the **JVM**, **WebAssembly**, **Kotlin/Native** and **Android**, with Maven dependencies, a lockfile, built-in frameworks, tasks and plugins.

```console
$ kargo new hello && cd hello
     Created application `hello` package
$ kargo run
   Compiling hello v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.47s
     Running `java com.example.hello.MainKt`
Hello, Kotlin!
$ kargo test
```

## How it works

* **No Gradle for JVM, Wasm and Native.** Kargo downloads `kotlin-compiler-embeddable` and `kotlin-native-prebuilt` from Maven Central and drives the compilers directly. Kargo can also download the JDK for you (`[toolchain] jdk = "21"`), so beyond Kargo itself you only need Node.js, and only to run Wasm programs.
* **Real Maven resolution.** POM inheritance, properties, BOM imports, exclusions, version ranges, relocations, and **Gradle module metadata**, so Kotlin Multiplatform libraries resolve to the right `.jar` or `.klib` for each target. Conflicts are resolved like Gradle: the highest version wins, unless `force = true`.
* **`kargo.lock`** pins every resolved version and the SHA-256 of every artifact. Checksums are verified against `.sha1` files on download and against the lockfile on re-download.
* **Incremental:** compilation is skipped when sources, classpath and flags are unchanged.
* **Android** is built by the Android Gradle Plugin. Kargo generates the Gradle project under `target/android` from `kargo.toml`, so you never edit Gradle files.

## Commands

| Command | Description |
|---|---|
| `kargo new <dir> [--lib] [-t jvm,wasm,native,android] [-w compose,ktor,...]` | Create a project |
| `kargo init [dir]` | Create a project in an existing directory |
| `kargo build [--release] [-t <target>]` | Compile all (or selected) targets |
| `kargo run [-t <target>] [-- args]` | Build and run |
| `kargo test [filter] [--no-run] [-t <target>]` | Compile and run tests |
| `kargo add <group:artifact[:version]>... [--dev] [-t <target>] [--scope runtime\|provided] [--bom]` | Add dependencies (latest release when no version is given) |
| `kargo add --framework serialization,coroutines` | Enable built-in frameworks |
| `kargo remove <group:artifact>...` | Remove dependencies |
| `kargo tree [-c compile\|runtime\|test] [--depth N] [--duplicates]` | Show the dependency graph |
| `kargo package [--release]` | Build a self-contained fat jar (JVM) |
| `kargo fetch` / `kargo update` | Pre-download everything / re-resolve `kargo.lock` |
| `kargo task [name] [args]`, `kargo <task>` | List or run tasks |
| `kargo plugins` | List compiler, hook and subcommand plugins |
| `kargo android <gradle args>` | Run any Gradle task in the generated Android project |
| `kargo toolchain list\|install\|remove\|available\|which` | Manage JDKs and Kotlin/Native distributions |
| `kargo clean`, `kargo info` | Remove `target/`, show project information |

Global flags: `-v/--verbose`, `-q/--quiet`, `--offline`, `--locked`, `--refresh`, `--manifest-path`.

## Project layout

```text
kargo.toml
kargo.lock
src/                 common sources (.kt, and .java on the JVM)
src-jvm/             target-specific sources: src-wasm/, src-native/, src-android/
tests/               common tests (kotlin.test)
tests-jvm/ ...       target-specific tests
resources/           JVM resources (test-resources/ for tests)
android/             AndroidManifest.xml, res/, assets/
target/              build output: target/<debug|release>/<target>/
```

`expect` declarations in `src/` can have `actual` implementations in `src-<target>/`. On the JVM, `.java` files next to your Kotlin files are compiled together with them (set `[target.jvm] main-class` if your `main` is in Java).

## `kargo.toml`

```toml
[package]
name = "my-app"
version = "0.1.0"
group = "com.example"

[kotlin]
version = "2.4.21"
opt-in = ["kotlin.ExperimentalStdlibApi"]
compiler-args = ["-Xcontext-parameters"]
# language-version, api-version, warnings-as-errors

# Targets: no [target] section means a JVM project.
[target.jvm]
jvm-target = "17"
# main-class = "com.example.MainKt"   # detected from a Kotlin `fun main`; set it for a Java main
# jvm-args = ["-Xmx1g"]

[target.wasm]
environment = "js"          # or "wasi"

[target.native]
platforms = ["host"]        # or linux_x64, macos_arm64, mingw_x64, ios_arm64, ...
kind = "executable"         # library, shared, static

[target.android]
namespace = "com.example.app"
min-sdk = 24                # target-sdk = 36, compile-sdk = 37, kind = "application" | "library"

[dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
guava = { group = "com.google.guava", version = "33.4.0-jre", exclude = ["com.google.code.findbugs:jsr305"] }
logback = { coords = "ch.qos.logback:logback-classic:1.5.32", scope = "runtime" }
"org.springframework.boot:spring-boot-dependencies" = { version = "4.1.1", bom = true }
# Other keys: classifier, extension, force, transitive

[target.jvm.dependencies]   # per-target dependencies

[dev-dependencies]          # test-only dependencies

[repositories]              # Maven Central is always used; Google Maven is added for Android/Compose
jitpack = "https://jitpack.io"
company = { url = "https://repo.example.com/maven", username-env = "REPO_USER", password-env = "REPO_PASS" }

[profile.release]
compiler-args = []          # also: debug, optimize
```

### Toolchains

```toml
[toolchain]
jdk = "21"            # newest Temurin 21; or an exact release like "21.0.5+11"; default "system"
```

With `jdk = "system"` (the default), Kargo uses `JAVA_HOME` or `java` on `PATH`. Any other value makes Kargo download that Temurin JDK from Adoptium on first use. The download is verified against the published SHA-256 and installed under `~/.kargo/toolchains/jdk/<release>`; it is then used for compiling, running and testing, and passed to Gradle for Android builds. A major version picks the newest installed release of it. Kargo also checks that `jvm-target` is not newer than the pinned JDK, and that the Kotlin compiler can run on it: compilers older than Kotlin 2.1.10 can't run on JDK 25 or newer, so pin `jdk = "21"` or upgrade Kotlin.

Kotlin/Native distributions are tied to `[kotlin] version`. They are installed under `~/.kargo/toolchains/kotlin-native/<version>` on the first native build. Their LLVM and sysroot dependencies go to `~/.kargo/toolchains/konan` instead of `~/.konan` (override with `KONAN_DATA_DIR`).

```console
$ kargo toolchain available          # JDK versions you can install (LTS marked)
$ kargo toolchain install jdk 21     # or: jdk 21.0.5+11, native 2.4.21
$ kargo toolchain install            # whatever the current project needs
$ kargo toolchain list               # installed JDKs/Kotlin/Native, disk usage, what the project uses
$ kargo toolchain which              # toolchains resolved for this project
$ kargo toolchain remove jdk 17      # all JDK 17 releases (or one release)
$ kargo toolchain remove native 2.4.10
```

### Built-in frameworks

```toml
[frameworks]
serialization = true        # or { version = "1.11.0", formats = ["json", "cbor", "protobuf"] }
coroutines = true           # or "1.11.0"
compose = true              # Compose Multiplatform (desktop JVM) / Jetpack Compose (Android)
ktor = { server = "netty", client = "cio", server-plugins = ["cors"], client-plugins = ["logging"] }
lombok = true               # Lombok for Java sources + the Kotlin Lombok plugin
spring-boot = { starters = ["web", "data-jpa"] }   # BOM, starters, allopen/noarg presets, -Xjsr305=strict
```

Each framework adds its libraries, compiler plugin and compiler flags for every target that supports it.

## Extending Kargo

### Tasks

```toml
[tasks]
fmt = "ktlint -F \"src/**/*.kt\""
gen = { run = ["mkdir -p build", "./gen.sh"], description = "Generate code", hooks = ["pre-build"] }
ci = { depends-on = ["fmt", "gen"] }
```

Run them with `kargo task ci` or just `kargo ci`. Dependencies run first, and cycles are rejected. Tasks run in the platform shell with `KARGO_*` variables set.

### Plugins

* **Compiler plugins** from Maven:
  ```toml
  [plugins.power-assert]
  maven = "org.jetbrains.kotlin:kotlin-power-assert-compiler-plugin-embeddable:2.4.21"
  id = "org.jetbrains.kotlin.powerassert"
  options = { function = "kotlin.assert" }
  ```
* **Hook plugins**: any executable, run at `pre-build`, `post-build`, `pre-test`, `post-test`, `pre-run` or `post-package`:
  ```toml
  [plugins.notify]
  command = "./scripts/notify.sh"   # paths are relative to the project; bare names use PATH
  args = ["--quiet"]                # the hook name is appended as the last argument
  hooks = ["post-build"]
  ```
  Hooks receive `KARGO_HOOK`, `KARGO_MANIFEST_DIR`, `KARGO_PKG_NAME`, `KARGO_PKG_VERSION`, `KARGO_PROFILE`, `KARGO_TARGET_DIR` and `KARGO_KOTLIN_VERSION`. After a JVM build they also get `KARGO_CLASSPATH` and `KARGO_JVM_JAR`, after `kargo package` they get `KARGO_PACKAGE`, and after tests they get `KARGO_TEST_RESULT`.
* **Subcommands**: an executable named `kargo-<name>` in `~/.kargo/bin` or on `PATH` becomes `kargo <name>`, just like Cargo.

## Environment

| Variable | Purpose |
|---|---|
| `KARGO_HOME` | Cache and toolchains (default `~/.kargo`) |
| `KARGO_TARGET_DIR` | Build output directory |
| `KARGO_OFFLINE` | Same as `--offline` |
| `KARGO_COMPILER_JVM_OPTS` | Extra JVM options for the Kotlin compiler |
| `KONAN_DATA_DIR` | Kotlin/Native dependency cache (default `~/.kargo/toolchains/konan`) |
| `JAVA_HOME`, `ANDROID_HOME` | JDK and Android SDK locations |

## Development

Kargo is a Cargo workspace:

| Crate | Responsibility |
|---|---|
| `kargo-util` | Terminal output, configuration, filesystem/hash/process helpers |
| `kargo-maven` | Coordinates, Maven versions and ranges, POMs, Gradle module metadata, downloads, resolver |
| `kargo-manifest` | The `kargo.toml` model, validation and format-preserving edits |
| `kargo-toolchain` | JDK detection and Temurin downloads, Kotlin compiler, Kotlin/Native, archives |
| `kargo-compiler` | JVM/Wasm/Native compiler drivers, jar packaging, entry points, test reports |
| `kargo-ops` | Build engine, lockfile, frameworks, tasks, plugins, Android/Gradle, commands |
| `kargo` | The CLI binary |

```bash
cargo test --workspace                       # unit + CLI integration tests (offline)
KARGO_E2E=1 cargo test --test e2e            # end-to-end: JVM, Wasm, Spring, Ktor, Compose, pinned JDK
KARGO_E2E=1 KARGO_E2E_NATIVE=1 KARGO_E2E_ANDROID=1 cargo test --test e2e
```
