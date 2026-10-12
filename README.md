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

* **No Gradle for JVM, Wasm and Native.** Kargo downloads `kotlin-compiler-embeddable` and `kotlin-native-prebuilt` from Maven Central and drives the compilers directly. Kargo can also download the JDK for you (`[toolchain] jdk = "21"`), so beyond Kargo itself you only need Node.js, and only to run Wasm programs (plus npm, pnpm or bun when a Wasm library needs npm packages, which Kargo installs for you).
* **Real Maven resolution.** POM inheritance, properties, BOM imports, exclusions, version ranges, relocations, and **Gradle module metadata**, so Kotlin Multiplatform libraries resolve to the right `.jar` or `.klib` for each target. Conflicts are resolved like Gradle: the highest version wins, unless `force = true`.
* **`kargo.lock`** pins every resolved version and the SHA-256 of every artifact. Checksums are verified against `.sha1` files on download and before a cached artifact is first locked, and against the lockfile on re-download. `-SNAPSHOT` versions resolve through the repository's `maven-metadata.xml` to the newest timestamped build, which the lockfile pins until `kargo update`.
* **Incremental:** a target is skipped when its sources, classpath and flags are unchanged. When something changed, only the affected Kotlin/JVM files are recompiled, in a warm compiler daemon, so an edit rebuilds in well under a second.
* **Android** is built by the Android Gradle Plugin. Kargo generates the Gradle project under `target/android` from `kargo.toml`, so you never edit Gradle files.

## Commands

| Command | Description |
|---|---|
| `kargo new <dir> [--lib] [-t jvm,wasm,native,android] [-w compose,ktor,...]` | Create a project |
| `kargo init [dir]` | Create a project in an existing directory |
| `kargo build [--release] [-t <target>] [--keep-going] [--message-format json]` | Compile all (or selected) targets; `--keep-going` builds every target and reports each failure; `--message-format json` prints compiler diagnostics as JSON |
| `kargo check [--tests] [--release] [-t <target>] [--keep-going]` | Type-check without packaging or linking |
| `kargo run [-t <target>] [--main <class> \| --bin <name> \| --example <name>] [-- args]` | Build and run; `--main` (alias `--entry-point` on Native) picks another `main`, `--bin` a `[[bin]]`, `--example` runs `examples/<name>.kt` |
| `kargo test [filter] [--no-run] [--no-fail-fast] [--report junit] [-t <target>]` | Compile and run tests; `filter` matches `package.Class.method` names (JVM, Native); `--no-fail-fast` tests every target and prints a result per target; `--report junit` writes JUnit XML to `target/<profile>/<target>/test-reports` |
| `kargo doc [--open] [-t <target>]` | Generate API documentation from KDoc with Dokka into `target/debug/doc` |
| `kargo fmt [--check]` | Format Kotlin sources with ktlint (`--check` only reports, for CI) |
| `kargo lint [--create-baseline]` | Analyze Kotlin sources with detekt |
| `kargo add --path <dir> [--dev] [-t <target>]` | Depend on another Kargo package on disk, built from source |
| `kargo add <group:artifact[:version]>... [--dev] [-t <target>] [--scope runtime\|provided\|processor\|ksp] [--bom]` | Add dependencies (latest release when no version is given) |
| `kargo add --framework serialization,coroutines` | Enable built-in frameworks |
| `kargo remove <group:artifact>... [--dev] [-t <target>]` | Remove dependencies |
| `kargo outdated [--pre]` | List dependencies in `kargo.toml` with newer compatible / latest versions |
| `kargo upgrade [<group:artifact>...] [--incompatible] [--dry-run]` | Raise versions in `kargo.toml`, preserving formatting |
| `kargo tree [-c compile\|runtime\|test] [--depth N] [--duplicates] [--format json]` | Show the dependency graph |
| `kargo package [--release] [--bin <name>]` | Build a self-contained fat jar (JVM); `--bin` launches that `[[bin]]` |
| `kargo fetch` | Pre-download everything |
| `kargo update [<group:artifact>...] [--precise <ver>] [--dry-run]` | Re-resolve `kargo.lock` (everything, or only the named modules) |
| `kargo task [name] [args]`, `kargo <task>` | List (also `kargo task --list` and `kargo --help`) or run tasks |
| `kargo plugins` | List compiler, hook and subcommand plugins |
| `kargo android <gradle args>` | Run any Gradle task in the generated Android project |
| `kargo toolchain list\|install\|remove\|available\|which` | Manage JDKs and Kotlin/Native distributions |
| `kargo daemon status\|stop` | Show or stop the background compiler daemons |
| `kargo clean`, `kargo info [--format json]` | Remove `target/` (except its `.kargo-lock`), show project information |
| `kargo completions <shell>` | Print a completion script (bash, zsh, fish, powershell, elvish) |

Global flags: `-v/--verbose`, `-q/--quiet`, `--offline`, `--locked`, `--refresh`, `--manifest-path`.

In a workspace, `build`, `check`, `test`, `doc`, `fmt`, `lint`, `tree` and `fetch` take `-p <member>` (repeatable) or `--workspace`; `run`, `package`, `add`, `remove`, `outdated` and `upgrade` take `-p <member>`.

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
interop/             cinterop .def files for C libraries (Kotlin/Native)
examples/            example programs: kargo run --example <name>
target/              build output: target/<debug|release>/<target>/
```

A workspace is a directory whose `kargo.toml` has a `[workspace]` with `members` (each a package like the one above). Members share the workspace's `kargo.lock` and `target/`, and build into `target/<profile>/<member>/<target>/`.

`expect` declarations in `src/` can have `actual` implementations in `src-<target>/`. On the JVM, `.java` files next to your Kotlin files are compiled together with them (set `[target.jvm] main-class` if your `main` is in Java).

Kargo warns about projects that build but are probably not what you meant: a target with no sources (`no Kotlin or Java sources found in src/ or src-jvm/ for the jvm target`), Kotlin scripts (`.kts`), which are never compiled or run, a task hidden by a built-in command, and a package version that doesn't look like a version (`not.a.version.x`). Warnings go to stderr, never change the exit code, and `-q` hides them.

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
# language-version, api-version, warnings-as-errors, ksp (KSP version, default 2.3.12)

# Targets: no [target] section means a JVM project.
[target.jvm]
jvm-target = "25"                     # 8 (or 1.8) and up, at most your JDK's version; default: 25, or your JDK's version if it is older
# main-class = "com.example.MainKt"   # detected from a top-level `fun main` or a `@JvmStatic fun main` in an object; set it for a Java main
# jvm-args = ["-Xmx1g"]
# javac-args = ["-Xlint:all"]         # extra javac options for Java sources

[target.wasm]
environment = "js"          # or "wasi"
# package-manager = "pnpm"  # npm, pnpm or bun for npm packages of Wasm libraries; default: first on PATH

[target.native]
platforms = ["host"]        # or linux_x64, macos_arm64, mingw_x64, ios_arm64, ...
kind = "executable"         # library, shared, static

[target.native.cinterop.sqlite]   # C library bindings; def defaults to interop/sqlite.def
package = "sqlite3"
linker-options = ["-lsqlite3"]    # also: def, headers, compiler-options

[target.android]
namespace = "com.example.app"
min-sdk = 24                # target-sdk = 36, compile-sdk = 37, kind = "application" | "library"

[dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
guava = { group = "com.google.guava", version = "33.4.0-jre", exclude = ["com.google.code.findbugs:jsr305"] }
logback = { coords = "ch.qos.logback:logback-classic:1.5.32", scope = "runtime" }
"org.springframework.boot:spring-boot-dependencies" = { version = "4.1.1", bom = true }
mathlib = { path = "../mathlib" }   # another Kargo package, built from source
lwjgl-natives = { coords = "org.lwjgl:lwjgl:3.3.6", classifier = "natives-windows", scope = "runtime" }  # a classifier jar, next to the module's own
# Other keys: classifier, extension, force, transitive, workspace = true
"com.google.auto.value:auto-value" = { version = "1.11.0", scope = "processor" }  # javac annotation processor
"com.squareup.moshi:moshi-kotlin-codegen" = { version = "1.15.2", scope = "ksp" }  # KSP processor (JVM, Android)
# Scopes: compile (default), runtime, provided, processor, ksp. Other keys: classifier, extension, force, transitive

[target.jvm.dependencies]   # per-target dependencies

[[bin]]                     # more programs: kargo run --bin migrate, kargo package --bin migrate
name = "migrate"
path = "src/tools/Migrate.kt"   # the file with its `main`; or main = "<JVM class>" / entry-point = "<native function>"

[dev-dependencies]          # test-only dependencies

[repositories]              # Maven Central is always used; Google Maven is added for Android/Compose
jitpack = "https://jitpack.io"
company = { url = "https://repo.example.com/maven", username-env = "REPO_USER", password-env = "REPO_PASS" }

[profile.release]
compiler-args = []          # also: debug, optimize

[doc]                       # kargo doc (Dokka); every key is optional
includes = ["docs/module.md"]
source-link = "https://github.com/me/my-app/blob/main"
external-links = ["https://kotlinlang.org/api/kotlinx.coroutines/"]

[fmt]                       # kargo fmt (ktlint); style rules go in .editorconfig
exclude = ["src/generated/**"]

[lint]                      # kargo lint (detekt); detekt.yml is picked up when present
config = ["config/detekt.yml"]
baseline = "detekt-baseline.xml"

[test]                      # kargo test
fail-fast = false           # test every target even if one fails (default: true)
```

### Workspaces

```toml
# kargo.toml at the workspace root: no [package]
[workspace]
members = ["app", "mathlib"]          # directories; one kargo.lock and one target/ for all

[workspace.kotlin]
version = "2.4.21"                    # every member builds with this Kotlin version

[workspace.dependencies]              # members inherit these with `name = { workspace = true }`
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
mathlib = { path = "mathlib" }
```

A path dependency brings its own Maven dependencies along, and is built for the consumer's target: a jar for the JVM, a klib for Wasm and Native (Android is not supported yet). Packages built together use one Kotlin version and one JDK.

### Toolchains

```toml
[toolchain]
jdk = "21"            # newest Temurin 21; or an exact release like "21.0.5+11"; default "system"
```

With `jdk = "system"` (the default), Kargo uses `JAVA_HOME` or `java` on `PATH`. Any other value makes Kargo download that Temurin JDK from Adoptium on first use. The download is verified against the published SHA-256 and installed under `~/.kargo/toolchains/jdk/<release>`; it is then used for compiling, running and testing, and passed to Gradle for Android builds. A major version picks the newest installed release of it. Kargo also checks that `jvm-target` is not newer than the JDK that builds the project (pinned or system), and that the Kotlin compiler can run on it: compilers older than Kotlin 2.1.10 can't run on JDK 25 or newer, so pin `jdk = "21"` or upgrade Kotlin.

Kotlin/Native distributions are tied to `[kotlin] version`. They are installed under `~/.kargo/toolchains/kotlin-native/<version>` on the first native build. Their LLVM and sysroot dependencies go to `~/.kargo/toolchains/konan` instead of `~/.konan` (override with `KONAN_DATA_DIR`). Debug builds link a precompiled standard library that the compiler builds on first use (one to two minutes) into the distribution's `klib/cache`, which roughly halves rebuild times; `KARGO_NATIVE_CACHE=false` turns it off.

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
proto = "protoc --kotlin_out=src-jvm proto/*.proto"
gen = { run = ["mkdir -p build", "./gen.sh"], description = "Generate code", hooks = ["pre-build"] }
ci = { depends-on = ["proto", "gen"], run = "kargo fmt --check" }
envs = { run = "echo $KARGO_PKG_NAME", shell = "sh" }                  # POSIX on every OS
dirs = { run = "ls", windows-run = "dir" }                               # per-OS commands
```

Run them with `kargo task ci` or just `kargo ci`. Built-in commands such as `kargo build` or `kargo fmt` take precedence over a task of the same name, which then runs only as `kargo task build`; Kargo warns about such a task, and `kargo task` and `kargo --help` mark it. Dependencies run first, and cycles are rejected. Tasks run in the platform shell (`sh` on Unix, `cmd` on Windows) with `KARGO_*` variables set. `shell = "sh"` runs a task in `sh` everywhere, using Git for Windows' `sh` on Windows (`bash`, `cmd`, `powershell` and `pwsh` are also accepted). On Windows, `windows-run` replaces `run` when it is set; otherwise `run` is used everywhere.

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
  windows-command = "./scripts/notify.bat"   # optional: used instead of `command` on Windows
  hooks = ["post-build"]
  ```
  `command` is run directly, not through a shell. On Windows a `.sh` script runs through `sh` (from Git for Windows); anything else must be a real executable (`.exe`, `.bat`, `.cmd`). `windows-command` and `windows-args` replace `command` and `args` on Windows; when they are not set, `command` and `args` are used everywhere.
  Hooks receive `KARGO_HOOK`, `KARGO_MANIFEST_DIR`, `KARGO_PKG_NAME`, `KARGO_PKG_VERSION`, `KARGO_PROFILE`, `KARGO_TARGET_DIR`, `KARGO_KOTLIN_VERSION` and `KARGO_BUILD_LOCK` (so a `kargo` call inside a hook doesn't wait on the build-directory lock its parent holds). After a JVM build they also get `KARGO_CLASSPATH` and `KARGO_JVM_JAR`, after `kargo package` they get `KARGO_PACKAGE`, and after tests they get `KARGO_TEST_RESULT`.
* **Subcommands**: an executable named `kargo-<name>` in `~/.kargo/bin` or on `PATH` becomes `kargo <name>`, just like Cargo.

## Environment

| Variable | Purpose |
|---|---|
| `KARGO_HOME` | Cache and toolchains (default `~/.kargo`) |
| `KARGO_TARGET_DIR` | Build output directory |
| `KARGO_OFFLINE` | Same as `--offline` |
| `KARGO_COMPILER_JVM_OPTS` | Extra JVM options for the Kotlin compiler daemon |
| `KARGO_DAEMON` | `false` turns off the compiler daemon |
| `KARGO_INCREMENTAL` | `false` turns off incremental compilation |
| `KARGO_NATIVE_CACHE` | `false` turns off the precompiled Kotlin/Native standard library cache for debug builds |
| `KONAN_DATA_DIR` | Kotlin/Native dependency cache (default `~/.kargo/toolchains/konan`) |
| `JAVA_HOME`, `ANDROID_HOME` | JDK and Android SDK locations |
| `BROWSER` | Browser for `kargo doc --open` |
| `COMPLETE` | Set by the `kargo completions` scripts; don't set it yourself |

## Development

Kargo is a Cargo workspace:

| Crate | Responsibility |
|---|---|
| `kargo-util` | Terminal output, configuration, filesystem/hash/process helpers |
| `kargo-maven` | Coordinates, Maven versions and ranges, POMs, Gradle module metadata, downloads, resolver |
| `kargo-manifest` | The `kargo.toml` model, validation and format-preserving edits |
| `kargo-toolchain` | JDK detection and Temurin downloads, Kotlin compiler, Kotlin/Native, archives |
| `kargo-compiler` | JVM/Wasm/Native compiler drivers, the compiler daemon, incremental compilation, jar packaging, entry points, test reports, compiler diagnostics |
| `kargo-ops` | Build engine, lockfile, frameworks, tasks, plugins, Android/Gradle, project warnings, commands |
| `kargo` | The CLI binary |

```bash
cargo test --workspace                       # unit + CLI integration tests (offline)
KARGO_E2E=1 cargo test --test e2e            # end-to-end: JVM, Wasm, Spring, Ktor, Compose, pinned JDK
KARGO_E2E=1 KARGO_E2E_NATIVE=1 KARGO_E2E_ANDROID=1 cargo test --test e2e
```
