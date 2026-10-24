# Kargo
[![Crates.io](https://img.shields.io/crates/v/kargo-cli.svg)](https://crates.io/crates/kargo-cli)

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

## Install

Prebuilt binaries need no Rust toolchain:

```sh
curl -fsSL https://enekocm7.github.io/kargo/install.sh | sh        # Linux and macOS
```

```powershell
irm https://enekocm7.github.io/kargo/install.ps1 | iex             # Windows
```

The scripts download the archive for your platform from the [GitHub release](https://github.com/enekocm7/kargo/releases), check its SHA-256, put `kargo` in `~/.kargo/bin` and offer to add that directory to your `PATH` (`KARGO_VERSION` picks a version, `--no-modify-path` leaves `PATH` alone). Releases have builds for Linux, macOS and Windows on x86_64 and aarch64, with `SHA256SUMS` and build provenance attestations. `nix run github:enekocm7/kargo` and `cargo binstall kargo-cli` work too; manifests for Homebrew, Scoop, winget and the AUR are built with each release but not yet listed in those registries. From source: `cargo install --locked kargo-cli` from crates.io, or `cargo install --locked --git https://github.com/enekocm7/kargo kargo-cli` for the latest commit (Rust 1.89 or newer; the crate is `kargo-cli`, the command is `kargo`). Update with `kargo self update`, remove with `kargo self uninstall`. See the [installation guide](https://enekocm7.github.io/kargo/getting-started/installation/).

## How it works

* **No Gradle for JVM, Wasm and Native.** Kargo downloads `kotlin-compiler-embeddable` and `kotlin-native-prebuilt` from Maven Central and drives the compilers directly. Kargo also downloads a JDK when you have none (or the one `[toolchain] jdk = "21"` pins), so beyond Kargo itself you only need Node.js, and only to run Wasm programs (plus npm, pnpm or bun when a Wasm library needs npm packages, which Kargo installs for you). A Wasm project can also target browsers (`environment = "browser"`: `kargo run` serves the page, `kargo test` uses a headless browser) or be an npm library (`kind = "library"`).
* **Real Maven resolution.** POM inheritance, properties, BOM imports, exclusions, version ranges, relocations, and **Gradle module metadata**, so Kotlin Multiplatform libraries resolve to the right `.jar` or `.klib` for each target. Conflicts are resolved like Gradle: the highest version wins, unless `force = true`.
* **`kargo.lock`** pins every resolved version and the SHA-256 of every artifact, and of the tools the build runs: the Kotlin compiler, compiler plugins, the JUnit launcher, KSP, Dokka, ktlint and detekt. Checksums are verified against the repository's `.sha256` (else `.sha1`) files on download and before a cached artifact is first locked (`--require-checksums` fails on artifacts with neither), and every artifact a build uses is checked against the lockfile, so a corrupt cached file fails the build instead of being used. npm packages for Wasm install with `--ignore-scripts`. `-SNAPSHOT` versions resolve through the repository's `maven-metadata.xml` to the newest timestamped build, which the lockfile pins until `kargo update`.
* **Incremental:** a target is skipped when its sources, classpath and flags are unchanged. When something changed, only the affected Kotlin/JVM files are recompiled, in a warm compiler daemon, so an edit rebuilds in well under a second.
* **Android** is built by the Android Gradle Plugin. Kargo generates the Gradle project under `target/android` from `kargo.toml`, so you never edit Gradle files, and downloads the Gradle version that plugin needs (a project `gradlew` wins; `gradle-version` pins another).

## Commands

| Command | Description |
|---|---|
| `kargo new <dir> [--lib] [-t jvm,wasm,native,android] [-w compose,ktor,...] [--bench] [--workspace]` | Create a project (inside a workspace, it joins the `members`), or with `--workspace` an empty workspace; `-w compose-multiplatform` shares a Compose `App()` between the desktop and the browser |
| `kargo init [dir]` | Create a project in an existing directory |
| `kargo import [dir] [--dry-run] [--no-build] [--from maven\|gradle]` | Convert a Maven or Gradle project: write the `kargo.toml` (a workspace for several modules), move the sources into Kargo's layout (`git mv` in a git repository), report what could not be translated and build the result; `--dry-run` prints the plan and changes nothing (see [importing a project](https://enekocm7.github.io/kargo/guides/importing/)) |
| `kargo init-ci [--system github] [--junit] [--force]` | Write `.github/workflows/ci.yml` from `kargo.toml`: the steps of the CI guide, `setup-java` unless `[toolchain] jdk` is set, `setup-node` for Wasm and an OS matrix for Kotlin/Native (see [continuous integration](https://enekocm7.github.io/kargo/guides/ci/)) |
| `kargo build [--release \| --profile <name>] [-t <target>] [--keep-going] [--xcframework] [-j <n>] [--message-format json]` | Compile all (or selected) targets; `--keep-going` builds every target (and every workspace member whose dependencies built) and reports each failure; `--xcframework` combines Apple frameworks into an XCFramework; `-j` sets how many workspace members build at once; `--message-format json` prints compiler diagnostics as JSON |
| `kargo check [--tests] [--release \| --profile <name>] [-t <target>] [--keep-going]` | Type-check without packaging or linking |
| `kargo run [<file>.kt \| <file>.kts \| -e <code>] [-t <target>] [--main <class> \| --bin <name> \| --example <name>] [--port <n>] [--open] [--debug[=<port>]] [--native-agent] [--flavor <name>] [--env-file <path>] [--no-env-file] [--watch \| --hot-reload [--clear] [--ignore <glob>]] [-- args]` | Build and run; `--main` (alias `--entry-point` on Native) picks another `main`, `--bin` a `[[bin]]`, `--example` runs `examples/<name>.kt`; a `.kt` file path compiles that file against the project it is in and runs it without adding it to the build, while a script, a `.kt` file with a front matter or one outside any project runs as `kargo script` runs it (`-e` too); for a Wasm browser app it serves the page on `127.0.0.1` (`--port`, default 8080) and `--open` opens it; `--debug` starts the JVM waiting for a JDWP debugger (port 5005); `--native-agent` runs it on GraalVM with the tracing agent, recording ``kargo package [--release | --profile <name>] [-t jvm|android] [--bin <name>] [--fat] [--format jar|jlink|native-image|installer] [--desktop [--platforms all]]`` configuration in `native-image/`; `--flavor` picks the Android product flavor to install; `--watch` rebuilds and restarts the program when its files change (a failed build leaves the running one alone), see [watch mode](https://enekocm7.github.io/kargo/guides/watch/); `--hot-reload` keeps a Compose Desktop app running and swaps the changed code into it, see [hot reload](https://enekocm7.github.io/kargo/guides/compose/#hot-reload) |
| `kargo script <file> [args]...` / `kargo script -e <code> [args]...` | Run a single Kotlin file without a project: a `.kts` script with `@file:DependsOn` and other `@file:` annotations, or a `.kt` file with a `/* kargo ... */` front matter; dependencies resolve and cache like a project's, and a second run of an unchanged script starts without compiling; `-e` runs code from the command line; `--` separates arguments that start with `-` |
| `kargo test [filter] [--no-run] [--no-fail-fast] [--report junit] [--debug[=<port>]] [--list [--format json]] [--env-file <path>] [--no-env-file] [-t <target>] [--watch [--clear] [--ignore <glob>]]` | Compile and run tests; `--watch` runs them again on every change; `filter` matches `package.Class.method` names (JVM, Native); `--no-fail-fast` tests every target and prints a result per target; `--report junit` writes JUnit XML to `target/<profile>/<target>/test-reports`; `--debug` waits for a debugger before running the JVM tests; `--list` prints the tests without running them (JVM, Native); `--message-format json` reports each test as JSON |
| `kargo bench [filter] [--no-run] [--save <name>] [--baseline <name>] [--format json] [--warmup <NxT>] [--iterations <NxT>] [--forks <n>] [--mode <mode>] [--time-unit <unit>] [--env-file <path>] [--no-env-file] [--profile <name>]` | Run the JVM benchmarks in `benches/` with JMH (kotlinx-benchmark annotations), always optimized (the `release` profile, or `[profile.bench]`); `--save` stores the results as a baseline in `target/bench/`, `--baseline` compares with one (`1.20 ms -> 1.05 ms (-12.5%, p < 0.05)`); `--format json` prints JMH's JSON results |
| `kargo watch [-x <command>]... [command] [--clear] [--ignore <glob>]` | Run a kargo command again whenever the project's files change, such as `kargo watch check` or `kargo watch -x "test -t jvm" -x run`; the commands run in order, and `--ignore` takes gitignore-style patterns |
| `kargo doc [--open] [-t <target>]` | Generate API documentation from KDoc with Dokka into `target/debug/doc` |
| `kargo preview [<name>...] [--list] [--width <dp>] [--height <dp>] [--density <n>] [--out-dir <dir>]` | Render the `@Preview` composables of a Compose Desktop app to PNG files in `target/preview`, off screen |
| `kargo fmt [--check]` | Format Kotlin sources with ktlint (`--check` only reports, for CI) |
| `kargo lint [--create-baseline]` | Analyze Kotlin sources with detekt |
| `kargo add --path <dir> [--dev] [-t <target>]` | Depend on another Kargo package on disk, built from source |
| `kargo add <group:artifact[:version]>... [--dev] [-t <target>] [--scope runtime\|provided\|processor\|ksp] [--bom]` | Add dependencies (latest release when no version is given) |
| `kargo add --framework serialization,coroutines` | Enable built-in frameworks |
| `kargo add --cinterop <name> [--header <file>] [--pkg-config <package>]` | Add a C library: `[target.native.cinterop.<name>]` and `interop/<name>.def` |
| `kargo remove <group:artifact>... [--dev] [-t <target>]` | Remove dependencies |
| `kargo target add\|remove\|list <target>... [--lib\|--app]` | Enable or disable targets in an existing project (keeps `kargo.toml` formatting, scaffolds `src-<target>/` and Android files), or list them |
| `kargo outdated [--pre]` | List dependencies in `kargo.toml` with newer compatible / latest versions |
| `kargo upgrade [<group:artifact>...] [--incompatible] [--dry-run]` | Raise versions in `kargo.toml`, preserving formatting |
| `kargo tree [-c compile\|runtime\|test] [--depth N] [--duplicates] [--format json]` | Show the dependency graph |
| `kargo package [--release \| --profile <name>] [-t jvm\|android] [--bin <name>] [--fat] [--format jar\|jlink\|native-image\|installer] [--desktop [--platforms all]]` | Build a self-contained fat jar (JVM); `--bin` launches that `[[bin]]`. A library gets its own jar and sources jar instead (`--fat` for the fat jar). `--format jlink` builds the jars, a Java runtime trimmed by `jlink` and launchers, which run without an installed Java; `native-image` a GraalVM native executable; `installer` a `jpackage` installer (`.msi`, `.dmg`, `.deb`, ...) for the host OS, all into `target/<profile>/jvm/package/` (see [distributing applications](https://enekocm7.github.io/kargo/guides/distributing/)). `--desktop` packages a [Compose Desktop](https://enekocm7.github.io/kargo/guides/compose/) app: an installer with what Compose needs (signed and notarized on macOS), or with `--format jar --platforms all` a jar that runs on every OS. `-t android` builds the (signed) App Bundles and APKs, or AARs, of every flavor into `target/<profile>/android/` |
| `kargo publish [--repository <name> \| --local] [--dry-run]` | Publish a library (POM, Gradle module metadata, sources/javadoc jars, checksums, GnuPG signatures) to Maven Central, a `[repositories]` entry or `~/.m2` |
| `kargo install --path <dir> \| --git <url> [--tag \| --branch \| --rev] \| <group:artifact[:version]> [--bin <name>] [--format jar\|jlink] [--force] [--list]` | Install a Kotlin command-line tool: build a project (release profile), a git repository or download an application from Maven, and put a launcher in `~/.kargo/bin`. A JVM tool runs its fat jar with the JDK it was built with (`--format jlink`: with a trimmed runtime, so no Java is needed); a Native tool is copied as it is. Every entry point is installed, or the `--bin` ones. `kargo install --force <name>` upgrades it from where it came from, `--list` shows what is installed, `--root <dir>` installs elsewhere (see [installing tools](https://enekocm7.github.io/kargo/guides/installing-tools/)) |
| `kargo uninstall <name>...` | Remove programs installed with `kargo install` |
| `kargo fetch [--sources] [--tools]` | Pre-download everything; `--sources` also downloads the libraries' sources jars for editors, `--tools` the Dokka, ktlint and detekt tools |
| `kargo generate [name...] [-t <target>] [--release \| --profile <name>] [--force]` | Run the `[[generate]]` steps of `kargo.toml` without building: protobuf and gRPC, OpenAPI clients, a `BuildConfig`, any tool that writes sources. A step writes into `target/generated/<profile>/<name>/` (`$KARGO_GEN_DIR`), which is compiled with the sources, and runs again only when its `inputs` change. `build`, `check`, `test`, `run` and `package` run the steps they need first (see [code generation](https://enekocm7.github.io/kargo/guides/code-generation/)) |
| `kargo vendor [path] [--toolchains] [--sync]` | Copy every dependency (and with `--toolchains` the Kotlin compiler and Kotlin/Native) into `vendor/` as a Maven repository; `kargo build --vendored` (or `[source] vendored = true` in `.kargo/config.toml`) then resolves from it alone (see [offline and corporate builds](https://enekocm7.github.io/kargo/guides/offline-and-mirrors/)) |
| `kargo classpath [-t target] [-s compile\|runtime\|test] [--format path\|lines\|json]` | Print the resolved jars (or klibs) of a target, the ones the build compiles with |
| `kargo ide [--kls\|--zed]` | Write `workspace.json` (Kotlin LSP) or `kls-classpath` (kotlin-language-server) for editors that run the server themselves, or `.zed/tasks.json` with Kargo tasks for Zed's run buttons; kept current by later commands |
| `kargo metadata [--no-deps] [--fingerprint]` | Print the project as versioned JSON for editors and language servers: roots, classpaths, compiler arguments and plugins, entry points, tasks |
| `kargo lsp [--server kargo\|kotlin-lsp\|kls\|none]` | Language server for editors over stdio: `kargo.toml` diagnostics, completion, hover (including Maven coordinates, versions and newer-version hints) and code actions (quick fixes, add a dependency, framework or target, upgrade), `kargo/runnables`, Kotlin code intelligence (completion, hover, signature help, go to definition and implementation, references, call and type hierarchies, rename, semantic highlighting, inlay hints, folding and selection ranges, unused code and deprecated usages as you type, formatting with ktlint, code actions: import, organize imports, add a missing dependency, implement members, add the missing branches of a `when`, add `actual` declarations, remove an unused import or variable, replace a deprecated call with its `ReplaceWith`; refactorings: extract and inline a variable or function, move or remove a parameter, move a declaration to another package or file) from Kargo's own Kotlin server, built in and aware of every target's source sets, standard library and libraries, or from a Kotlin language server Kargo downloads and keeps importing the project into (multiplatform packages as source sets, so `expect`/`actual` link up; `--klib-targets` adds Wasm and Native), and `kargo check` on save (`checkOnSave`, on with Kargo's own server) with errors for every target |
| `kargo dap` | Debug adapter for editors over stdio: kotlin-debug-adapter on the project's JDK, launching `kargo run` or `kargo test` with `--debug` and mapping Kargo's source layout so breakpoints and stack frames find your files |
| `kargo update [<group:artifact>...] [--precise <ver>] [--dry-run]` | Re-resolve `kargo.lock` (everything, or only the named modules) |
| `kargo task [--env-file <path>] [name] [args]`, `kargo <task>` | List (also `kargo task --list` and `kargo --help`) or run tasks; `--env-file` picks the `.env` files of a package task |
| `kargo env [--release \| --profile <name>] [--show-values] [--check]` | Show the environment `kargo run` and `kargo test` use: each variable, where it comes from, and its value (hidden unless `--show-values`); `--check` fails on unset required variables and on keys of `.env.example` that nothing sets |
| `kargo plugins` | List compiler, hook and subcommand plugins |
| `kargo android <gradle args>` | Run any Gradle task in the generated Android project |
| `kargo toolchain list\|install\|remove\|available\|which` | Manage JDKs, GraalVMs, Kotlin/Native and Gradle distributions, the Kotlin language servers behind `kargo lsp` and the debug adapter behind `kargo dap` |
| `kargo self update [--version <v>] [--check]` | Update Kargo to the latest release (or `--version`), checking the archive's SHA-256 against the release's `SHA256SUMS`; `--check` only says whether a newer one exists. A Kargo installed by Homebrew, Scoop, winget, Nix, SDKMAN!, Cargo or a system package manager is not replaced: its update command is printed instead. `KARGO_RELEASES_URL` points at a mirror |
| `kargo self uninstall [--purge]` | Remove the binary and the `PATH` entries the install scripts added; `--purge` also deletes `~/.kargo` |
| `kargo daemon status\|stop` | Show or stop the background compiler daemons |
| `kargo clean`, `kargo info [--format json]` | Remove `target/` (except its `.kargo-lock`; `--release` or `--profile` only that profile's output and generated files), show project information |
| `kargo completions <shell>` | Print a completion script (bash, zsh, fish, powershell, elvish) |

Global flags: `-v/--verbose`, `-q/--quiet`, `--offline`, `--locked`, `--vendored`, `--refresh`, `--require-checksums`, `--manifest-path`.

Behind a company firewall, `~/.kargo/config.toml` (or a project's `.kargo/config.toml`) sends downloads through mirrors and a proxy:

```toml
[mirrors]
"*" = "https://artifactory.corp/maven-virtual"   # replaces Maven Central, Google and [repositories]

[mirrors.credentials."https://artifactory.corp"]
username-env = "ART_USER"
password-env = "ART_TOKEN"

[toolchain-mirrors]                              # JDK, GraalVM and Gradle downloads, by URL prefix
"https://services.gradle.org/distributions" = "https://artifactory.corp/gradle"

[http]
proxy = "http://proxy.corp:3128"                 # else HTTPS_PROXY / NO_PROXY
cainfo = "/etc/ssl/certs/corp-ca.pem"
```

`kargo --help` lists the common commands, then the project's tasks, and `kargo` alone prints the same help; `kargo --list` lists every built-in command with its description. Commands left out of `--help` still work, and `kargo help <command>` shows each one's options. A mistyped command suggests the closest one (`kargo biuld` → `Did you mean build?`).

In a workspace, `build`, `check`, `test`, `doc`, `fmt`, `lint`, `tree`, `fetch` and `publish` take `-p <member>` (repeatable), `--workspace` and `--exclude <member>`; `run`, `package`, `install`, `add`, `remove`, `target`, `outdated` and `upgrade` take `-p <member>`. Members build in dependency order, independent ones in parallel, and a JVM member recompiles only when the ABI of a member it uses changes.

## Project layout

```text
kargo.toml
kargo.lock
src/                 common sources (.kt, and .java on the JVM)
src-jvm/             target-specific sources: src-wasm/, src-native/, src-android/
tests/               common tests (kotlin.test)
tests-jvm/ ...       target-specific tests
test-fixtures/       test helpers the tests of dependent packages can use (`test-fixtures = true`)
benches/             common benchmarks for kargo bench (JVM); benches-jvm/ for JVM-only ones
resources/           JVM resources (test-resources/ for tests); the files and index.html of a Wasm browser app
composeResources/    Compose images, fonts, strings and files, read through the generated Res class
android/             AndroidManifest.xml, res/, assets/
interop/             cinterop .def files for C libraries (Kotlin/Native)
c/                   C/C++ sources compiled into the native binaries ([target.native.c-sources])
examples/            example programs: kargo run --example <name>
target/              build output: target/<debug|release|name>/<target>/ (a custom profile's name);
                     target/generated/<profile>/<step>/ holds what [[generate]] steps write
```

A workspace is a directory whose `kargo.toml` has a `[workspace]` with `members` (each a package like the one above, or a glob such as `libs/*`). Members share the workspace's `kargo.lock` and `target/`, and build into `target/<profile>/<member>/<target>/`.

`expect` declarations in `src/` can have `actual` implementations in `src-<target>/`. On the JVM, `.java` files next to your Kotlin files are compiled together with them (set `[target.jvm] main-class` if your `main` is in Java).

Kargo warns about projects that build but are probably not what you meant: a target with no sources (`no Kotlin or Java sources found in src/ or src-jvm/ for the jvm target`), Kotlin scripts (`.kts`), which are never compiled or run, a task hidden by a built-in command, and a package version that doesn't look like a version (`not.a.version.x`). Warnings go to stderr, never change the exit code, and `-q` hides them.

## `kargo.toml`

```toml
[package]
name = "my-app"
version = "0.1.0"
group = "com.example"
# description, authors = ["Ada <ada@example.com>"], license = "MIT", homepage, repository: the POM of `kargo publish`
# publish = false           # keep `kargo publish` away from this package

# [package.`kargo package [--release | --profile <name>] [-t jvm|android] [--bin <name>] [--fat] [--format jar|jlink|native-image|installer] [--desktop [--platforms all]]`]           # kargo package --format jlink / installer
# modules = ["java.naming"] # JDK modules jdeps can't see (reflection, service loaders)

# [package.`kargo package [--release | --profile <name>] [-t jvm|android] [--bin <name>] [--fat] [--format jar|jlink|native-image|installer] [--desktop [--platforms all]]`]    # kargo package --format native-image
# args = ["--enable-url-protocols=https"]   # also: static (musl, Linux), initialize-at-build-time, metadata-repository

# [package.installer]       # kargo package --format installer / --desktop (jpackage)
# vendor = "Example Inc."   # also: name, description, app-version, type, icon, license-file, menu, shortcut,
#                           # console, upgrade-uuid (Windows), bundle-id, signing-identity (macOS), category (Linux),
#                           # file-associations, notarize = { apple-id-env, password-env, team-id } (macOS)
# type = ["msi", "dmg", "deb"]   # each OS builds its own

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
environment = "js"          # or "browser" (serves a page, tests in a headless browser) or "wasi"
# kind = "library"          # an npm package of @JsExport functions: package.json, .mjs, .wasm, .d.mts
# module-name = "hello"     # name of the .mjs and .wasm files; default: the package name
# npm-name = "@acme/hello"  # npm name of a library; default: the lowercase package name
# package-manager = "pnpm"  # npm, pnpm or bun for npm packages of Wasm libraries; default: first on PATH
# wasm-opt = false          # release builds run wasm-opt from PATH if there is one; or the path of one
# web-resources = ["web/raylib.mjs", "web/raylib.wasm"]  # a library's files, copied next to every browser app using it
# web-init = "web/init.mjs" # a library's script the generated page imports before the app module

# [target.wasm.npm-dependencies]
# "left-pad" = "1.3.0"      # npm packages besides those your Wasm libraries ask for; Kotlin gets declarations from their .d.ts

[target.native]
platforms = ["host"]        # or linux_x64, macos_arm64, mingw_x64, ios_arm64, ...
kind = "executable"         # library, shared, static, framework (Apple; kargo build --xcframework)

[target.native.platform.mingw_x64]  # linker options for one platform
linker-options = ["-lws2_32"]

[target.native.c-sources]   # compile c/**/*.c with the Kotlin/Native clang and link it in

[target.native.cinterop.sqlite]   # C library bindings; def defaults to interop/sqlite.def
headers = ["sqlite3.h"]           # no .def file needed
package = "sqlite3"
pkg-config = "sqlite3"            # flags from sqlite3.pc; link = "static" embeds the library
# also: def, compiler-options, linker-options, static-libraries, library-paths, frameworks,
#       header-filter, excluded-functions, strict-enums, modules, and platform.<platform> tables

[target.android]
namespace = "com.example.app"
min-sdk = 24                # target-sdk = 36, compile-sdk = 37, kind = "application" | "library"
# agp-version = "9.4.1"     # gradle-version defaults to what AGP needs (9.6.0)
# minify = true             # R8 for release builds; proguard-files = ["android/proguard-rules.pro"]
# build-config = { API_URL = "https://api.example.com" }   # BuildConfig fields

# [target.android.signing.release]  # passwords come from environment variables
# store-file = "release.jks"
# store-password-env = "KS_PASS"
# key-alias = "upload"

# [target.android.flavors.free]     # product flavors: kargo run -t android --flavor free
# dimension = "tier"
# application-id-suffix = ".free"

[dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
guava = { group = "com.google.guava", version = "33.4.0-jre", exclude = ["com.google.code.findbugs:jsr305"] }
logback = { coords = "ch.qos.logback:logback-classic:1.5.32", scope = "runtime" }
"org.springframework.boot:spring-boot-dependencies" = { version = "4.1.1", bom = true }
mathlib = { path = "../mathlib" }   # another Kargo package, built from source
lwjgl-natives = { coords = "org.lwjgl:lwjgl:3.3.6", classifier = "natives-windows", scope = "runtime" }  # a classifier jar, next to the module's own
# Other keys: classifier, extension, force, transitive, workspace = true, exported = true (part of the API of this package)
"com.google.auto.value:auto-value" = { version = "1.11.0", scope = "processor" }  # javac annotation processor
"com.squareup.moshi:moshi-kotlin-codegen" = { version = "1.15.2", scope = "ksp" }  # KSP processor (JVM, Android)
# Scopes: compile (default), runtime, provided, processor, ksp. Other keys: classifier, extension, force, transitive

[target.jvm.dependencies]   # per-target dependencies

[[bin]]                     # more programs: kargo run --bin migrate, kargo package --bin migrate
name = "migrate"
path = "src/tools/Migrate.kt"   # the file with its `main`; or main = "<JVM class>" / entry-point = "<native function>"

[[generate]]                # code written before compiling, from other files (kargo generate); output: target/generated/<profile>/<name>/
name = "protos"
builtin = "protobuf"        # downloads protoc; grpc = true adds the gRPC stubs. Or run = "<command>", or tool = "group:artifact:version"
inputs = ["proto/**/*.proto"]   # the step runs again only when these change (default for protobuf)
# builtin = "build-config" writes an object with VERSION, GROUP, NAME, GIT_SHA and PROFILE, plus fields = { API_URL = "..." }

[dev-dependencies]          # test-only dependencies

[repositories]              # Maven Central is always used; Google Maven is added for Android/Compose
jitpack = "https://jitpack.io"
company = { url = "https://repo.example.com/maven", username-env = "REPO_USER", password-env = "REPO_PASS" }
local = "maven-local"       # ~/.m2/repository, where `kargo publish --local` and `mvn install` put libraries

[publish]                   # kargo publish; every key is optional
repository = "maven-central"  # default destination: maven-central, maven-local or a [repositories] entry
# sign = true               # GnuPG signatures; default: on for Maven Central only
# sources = true
# javadoc = "dokka"         # or "empty" (a manifest only) or false
# central-publishing = "manual"   # or "automatic": release as soon as Maven Central validates the upload

[env]                       # variables for kargo run/test, tasks and hooks; .env files and the shell override them
APP_NAME = "demo"
DATABASE_URL = { required = true }   # fails early unless the shell or a .env file sets it

[profile.release]
compiler-args = []          # also: debug, optimize

[profile.staging]           # kargo build --profile staging
inherits = "release"        # the profile it starts from (default "dev")
jvm-args = ["-Xmx1g"]       # for kargo run, test and bench; also: dependencies, env

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
timeout = "10m"             # stop the whole test run after this long (optional)
test-timeout = "30s"        # fail a JVM test that runs longer than this (optional)
jvm-args = ["-Xmx1g"]       # extra JVM arguments for the test process

[bench]                     # kargo bench (JMH); every key is optional
warmup = "5x1s"             # warmup iterations: a count and how long each lasts (default 3x1s)
iterations = "5x1s"         # measured iterations (default 5x1s)
forks = 2                   # JVMs per benchmark (default 2; 0 runs in the launcher's JVM)
mode = "avgt"               # thrpt, avgt, sample, ss or all (default avgt)
time-unit = "us"            # ns, us, ms, s, m, h or d (default: whichever reads best)
jvm-args = ["-Xmx2g"]       # extra JVM arguments for the benchmark JVMs
```

Editors complete and validate `kargo.toml` with its JSON Schema, `https://enekocm7.github.io/kargo/schemas/kargo.json`: put `#:schema <url>` on the first line for Taplo-based editors (Even Better TOML, Zed, Helix), or map the URL to `kargo.toml` in your editor. See [Editor integration](https://enekocm7.github.io/kargo/guides/editors/).

IntelliJ IDEA and Android Studio are the best supported editors, since Kotlin is JetBrains' own language. Their plugin in [`editors/jetbrains`](editors/jetbrains) that imports the project from `kargo metadata` (modules, source roots and the libraries of `kargo.toml`, including Kotlin/Native and Wasm klibs and the npm packages of Wasm targets) so the IDE resolves your dependencies, runs `kargo lsp` for `kargo.toml` and adds a New Project wizard, run configurations, Run/Debug gutter icons, a test tree, JVM debugging and a Kargo tool window. See [Editor integration](https://enekocm7.github.io/kargo/guides/editors/#intellij-idea-and-android-studio).

VS Code gets an extension in [`editors/vscode`](editors/vscode) that runs `kargo lsp` for Kotlin and `kargo.toml`, runs the `kargo` commands as tasks with their compiler errors in the Problems view, shows the tests in a test explorer, puts Run and Debug buttons above `fun main` and the tests, and debugs through `kargo dap`; see [Editor integration](https://enekocm7.github.io/kargo/guides/editors/#visual-studio-code). Zed gets an extension in [`editors/zed`](editors/zed) that runs `kargo lsp` for Kotlin and `kargo.toml`, debugs programs and tests through `kargo dap`, and adds a `/kargo-metadata` slash command; `kargo ide --zed` writes the tasks behind its run buttons. Neovim gets a plugin in [`editors/neovim`](editors/neovim) that runs `kargo lsp` for Kotlin and `kargo.toml`, runs the Kargo commands with their errors in the quickfix list, lists the runnables of a package, adds a neotest adapter for the tests and sets up nvim-dap for debugging; see [Editor integration](https://enekocm7.github.io/kargo/guides/editors/#neovim). Emacs gets a package in [`editors/emacs`](editors/emacs) that runs `kargo lsp` from Eglot or lsp-mode, runs `kargo build`, `check`, `run` and `test` through `compile` with their errors linked to the source, runs a test, a class or a file and lists the results, shows build errors in Flymake, debugs through Dape and adds a Transient menu; see [Editor integration](https://enekocm7.github.io/kargo/guides/editors/#emacs). Other editors run `kargo lsp` for Kotlin and `kargo.toml`; see [Editor integration](https://enekocm7.github.io/kargo/guides/editors/#other-editors). Outside IntelliJ IDEA, Kotlin code is served by [Kargo's own Kotlin server](https://enekocm7.github.io/kargo/guides/kotlin-server/), written in Rust and built into `kargo`: completion, hover, go to definition, references, rename and formatting (ktlint, as `kargo fmt`) against your sources (every target's source sets), each target's standard library, the JDK's sources and your dependencies (from their sources jars, which it downloads in the background, or from their compiled classes and klibs until then and when they have none), with `kargo check` errors on save. `kargo lsp --server kotlin-lsp` runs JetBrains' Kotlin LSP instead.

### Workspaces

```toml
# kargo.toml at the workspace root: no [package]
[workspace]
members = ["apps/*", "libs/*"]        # directories or globs; one kargo.lock and one target/ for all
exclude = ["libs/attic"]
default-members = ["apps/server"]     # what commands work on at the root (default: every member)

[workspace.package]                   # members inherit these with `version.workspace = true`
version = "1.2.0"
group = "com.example"

[workspace.kotlin]
version = "2.4.21"                    # every member builds with this Kotlin version
opt-in = ["kotlin.RequiresOptIn"]     # also language-version, compiler-args, jvm-target, ...

[workspace.dependencies]              # members inherit these with `name = { workspace = true }`
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
mathlib = { path = "libs/mathlib" }

[workspace.frameworks]                # and these with `serialization.workspace = true` in [frameworks]
serialization = true

[workspace.tasks]                     # `kargo <task>` at the root; [workspace.plugins] work like [plugins]
release-notes = "./tools/notes.sh"
```

A path dependency is built for the consumer's target: a jar for the JVM, a klib for Wasm and Native, and for Android a Gradle subproject (an Android library) or the JVM jar. It brings its own Maven dependencies: the `exported = true` ones (its API) to compile against, the others at runtime only. Test helpers in a package's `test-fixtures/` reach the tests of packages that depend on it with `test-fixtures = true`. `kargo new <dir>` inside a workspace adds the member, and `kargo new --workspace` creates one. Packages built together use one Kotlin version and one JDK.

### Toolchains

```toml
[toolchain]
jdk = "21"            # newest Temurin 21; or an exact release like "21.0.5+11"; default "system"
graalvm = "25"        # GraalVM CE for native images (or "oracle-25"); default "system" (GRAALVM_HOME, then PATH)
kargo = ">=0.3"       # the Kargo versions the project builds with; an older Kargo stops and points to `kargo self update`
```

With `jdk = "system"` (the default), Kargo uses `JAVA_HOME` or `java` on `PATH`; on a machine with neither, it downloads Temurin 21 (or the LTS the project's `jvm-target` and Kotlin version need) into `~/.kargo/toolchains/jdk/` and uses that, except with `--offline` or `--locked`. Every toolchain a command needs installs on first use like this; `KARGO_AUTO_INSTALL=0` or `[toolchain] auto-install = false` in `config.toml` turns it off, and the error then names the `kargo toolchain install` command to run (see [what installs automatically](https://enekocm7.github.io/kargo/guides/toolchains/#what-installs-automatically)). Any other value makes Kargo download that Temurin JDK from Adoptium on first use. The download is verified against the published SHA-256 and installed under `~/.kargo/toolchains/jdk/<release>`; it is then used for compiling, running and testing, and passed to Gradle for Android builds. A major version picks the newest installed release of it. Kargo also checks that `jvm-target` is not newer than the JDK that builds the project (pinned or system), and that the Kotlin compiler can run on it: compilers older than Kotlin 2.1.10 can't run on JDK 25 or newer, so pin `jdk = "21"` or upgrade Kotlin.

`graalvm` is only used by `kargo package --format `kargo package [--release | --profile <name>] [-t jvm|android] [--bin <name>] [--fat] [--format jar|jlink|native-image|installer] [--desktop [--platforms all]]`` and `kargo run --native-agent`. Kargo looks the release up with the foojay Disco API, downloads it on first use, verifies its SHA-256 and installs it under `~/.kargo/toolchains/graalvm/<distribution>-<version>`, next to the GraalVM reachability metadata repository that native images of the dependencies use.

Kotlin/Native distributions are tied to `[kotlin] version`. They are installed under `~/.kargo/toolchains/kotlin-native/<version>` on the first native build. Their LLVM and sysroot dependencies go to `~/.kargo/toolchains/konan` instead of `~/.konan` (override with `KONAN_DATA_DIR`); `kargo toolchain install` downloads them for every `[target.native]` platform the host can build, so later builds work offline. Debug builds link a precompiled standard library that the compiler builds on first use (one to two minutes) into the distribution's `klib/cache`, which roughly halves rebuild times; `KARGO_NATIVE_CACHE=false` turns it off.

Android builds run on a Gradle that Kargo downloads from `services.gradle.org` (checksum-verified, under `~/.kargo/toolchains/gradle/<version>`): the version the Android Gradle Plugin in use requires, or `[target.android] gradle-version`. A `gradlew` in the project root takes precedence, `--system-gradle` uses the `gradle` on `PATH`, and `target/android` gets a Gradle wrapper so it opens in Android Studio.

Toolchains are locked while they install, so concurrent Kargo processes sharing `~/.kargo` install each one once, and downloads retry transient network failures. See [Parallel installs and retries](https://enekocm7.github.io/kargo/guides/toolchains/#parallel-installs-and-retries).

```console
$ kargo toolchain available          # JDK versions you can install (LTS marked)
$ kargo toolchain install jdk 21     # or: jdk 21.0.5+11, graalvm 25, native 2.4.21, gradle 9.6.0, kotlin-lsp, kls, kotlin-debug-adapter
$ kargo toolchain install            # whatever the current project needs (a JDK too, when there is none; Kotlin/Native's LLVM and sysroots)
$ kargo toolchain list               # installed JDKs/GraalVMs/Kotlin/Native/Gradle, disk usage, what the project uses
$ kargo toolchain which              # toolchains resolved for this project
$ kargo toolchain remove jdk 17      # all JDK 17 releases (or one release)
$ kargo toolchain remove native 2.4.10
```

### Built-in frameworks

```toml
[frameworks]
serialization = true        # or { version = "1.11.0", formats = ["json", "cbor", "protobuf"] }
coroutines = true           # or "1.11.0"
compose = true              # Compose Multiplatform: desktop (JVM), web (Wasm browser), iOS (Native framework), Jetpack Compose (Android); Res from composeResources/
# compose = { stability-config = "compose-stability.conf", strong-skipping = true, feature-flags = [], metrics = true, reports = true }  # compiler options
ktor = { server = "netty", client = "cio", server-plugins = ["cors"], client-plugins = ["logging"] }
lombok = true               # Lombok for Java sources + the Kotlin Lombok plugin
spring-boot = { starters = ["web", "data-jpa"] }   # BOM, starters, allopen/noarg presets, -Xjsr305=strict
benchmark = true            # kotlinx-benchmark annotations and JMH for the benchmarks in benches/ (kargo bench)
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
  `[[generate]]` steps get the same variables plus `KARGO_GEN_DIR` (their output directory), `KARGO_GEN_ROOT`, `KARGO_GEN_NAME` and `KARGO_GEN_KIND`. Hooks receive `KARGO_HOOK`, `KARGO_MANIFEST_DIR`, `KARGO_PKG_NAME`, `KARGO_PKG_VERSION`, `KARGO_PROFILE`, `KARGO_TARGET_DIR`, `KARGO_KOTLIN_VERSION`, `KARGO_WORKSPACE_DIR` (in a workspace) and `KARGO_BUILD_LOCK` (so a `kargo` call inside a hook doesn't wait on the build-directory lock its parent holds). After a JVM build they also get `KARGO_CLASSPATH` and `KARGO_JVM_JAR`, after `kargo package` they get `KARGO_PACKAGE`, and after tests they get `KARGO_TEST_RESULT`.
* **Subcommands**: an executable named `kargo-<name>` in `~/.kargo/bin` or on `PATH` becomes `kargo <name>`, just like Cargo.

## Environment

| Variable | Purpose |
|---|---|
| `KARGO_HOME` | Cache and toolchains (default `~/.kargo`) |
| `KARGO_RELEASES_URL` | Where `kargo self update`, `install.sh` and `install.ps1` look for releases (default `https://github.com/enekocm7/kargo/releases`; a mirror, or a directory with `file://`) |
| `KARGO_NO_UPDATE_CHECK` | Any non-empty value stops `kargo --version` from mentioning newer releases (it checks at most once a day, never offline or in CI) |
| `KARGO_INSTALL_ROOT` | Where `kargo install` puts launchers (`<root>/bin`) and programs (`<root>/install`); default: `KARGO_HOME` |
| `KARGO_TARGET_DIR` | Build output directory |
| `KARGO_OFFLINE` | Same as `--offline` |
| `KARGO_REQUIRE_CHECKSUMS` | Same as `--require-checksums` |
| `KARGO_VENDORED` | Same as `--vendored` |
| `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`, `NO_PROXY` | Proxy for Kargo's downloads (overridden by `[http] proxy` in `config.toml`) |
| `KARGO_SYSTEM_GRADLE` | Same as `--system-gradle`: Android builds use the `gradle` on `PATH` |
| `KARGO_AUTO_INSTALL` | `0` stops commands from downloading the toolchains they need (a JDK when none is on `JAVA_HOME` or `PATH`, a pinned JDK or GraalVM, Kotlin/Native, Gradle, the language servers); `1` turns installs on over `[toolchain] auto-install = false` in `config.toml` |
| `KARGO_COMPILER_JVM_OPTS` | Extra JVM options for the Kotlin compiler daemon |
| `SOURCE_DATE_EPOCH` | The time (seconds since 1970) that a `build-config` generator with `build-time = true` writes as `BUILD_TIME`, instead of the current time |
| `KARGO_DAEMON` | `false` turns off the compiler daemon |
| `KARGO_INCREMENTAL` | `false` turns off incremental compilation |
| `KARGO_NATIVE_CACHE` | `false` turns off the precompiled Kotlin/Native standard library cache for debug builds |
| `KONAN_DATA_DIR` | Kotlin/Native dependency cache (default `~/.kargo/toolchains/konan`) |
| `JAVA_HOME`, `ANDROID_HOME` | JDK and Android SDK locations |
| `GRAALVM_HOME` | GraalVM for native images when `[toolchain] graalvm` is `system` (the default) |
| `BROWSER` | Browser for `kargo doc --open` and `kargo run --open` |
| `KARGO_BROWSER`, `KARGO_BROWSER_ARGS`, `KARGO_BROWSER_TIMEOUT` | Browser, extra arguments and silence timeout (seconds) for Wasm browser tests |
| `KARGO_MAVEN_LOCAL` | The local Maven repository for `kargo publish --local` and `"maven-local"` (default `~/.m2/repository`) |
| `KARGO_MAVEN_CENTRAL_USERNAME`, `KARGO_MAVEN_CENTRAL_PASSWORD` | Central Portal user token for `kargo publish` to Maven Central (`KARGO_MAVEN_CENTRAL_URL` overrides the Portal) |
| `KARGO_MAVEN_SEARCH_URL` | The Maven Central search API `kargo lsp` completes coordinates from |
| `KARGO_SIGNING_KEY`, `KARGO_SIGNING_KEY_PASSPHRASE`, `KARGO_SIGNING_KEY_ID`, `KARGO_GPG` | Signing key (ASCII-armored), its passphrase, which key, and the `gpg` to sign published files with |
| `COMPLETE` | Set by the `kargo completions` scripts; don't set it yourself |

## Development

Kargo is a Cargo workspace:

| Crate | Responsibility |
|---|---|
| `kargo-util` | Terminal output, configuration (including `config.toml`: mirrors, proxy, CA bundle, vendoring), the HTTP agent, filesystem/hash/process helpers |
| `kargo-maven` | Coordinates, Maven versions and ranges, POMs, Gradle module metadata, downloads, resolver, uploads and the Maven Central Portal |
| `kargo-manifest` | The `kargo.toml` model, validation and format-preserving edits |
| `kargo-toolchain` | JDK detection and Temurin downloads, GraalVM downloads and the reachability metadata repository, JetBrains Runtime for Compose hot reload, Kotlin compiler, Kotlin/Native, Gradle distributions, Kotlin language servers and the Kotlin debug adapter, browsers, archives |
| `kargo-compiler` | JVM/Wasm/Native compiler drivers, the compiler daemon, incremental compilation, ABI fingerprints of jars, jar packaging, entry points, test reports, compiler diagnostics |
| `kargo-ops` | Build engine, lockfile, frameworks, tasks, plugins, `[[generate]]` steps (`generate.rs`), Android/Gradle, project warnings, publications and signing, npm packages and their Kotlin declarations, Compose Desktop and Compose resources, `kargo self` (`ops/self_update.rs`), `kargo import` (`ops/import/`), commands |
| `kargo-lsp` | `kargo lsp`: `kargo.toml` diagnostics, completion and hover from the JSON Schema, dependency coordinate and version completion and update hints, code actions, Kargo's own Kotlin server (`kotlin/`: lexer, declaration parser, Java declarations and the JDK's sources, libraries without sources from class files, Kotlin metadata and klibs, index, resolver, completion, hover, signature help, navigation, implementations, call and type hierarchies, rename (also KDoc, labels, packages, files and `kargo.toml` entry points), KDoc links (navigation, hints) and generated KDoc, semantic tokens, inlay hints, folding and selection ranges, formatting, code actions, refactorings), project import for other Kotlin language servers, runnables, check on save; `kargo dap`, the debug adapter; Zed tasks for `kargo ide --zed` |
| `kargo` | The CLI binary |

```bash
cargo test --workspace                       # unit + CLI integration tests (offline)
KARGO_E2E=1 cargo test --test e2e            # end-to-end: JVM, Wasm, Spring, Ktor, Compose, pinned JDK
KARGO_E2E=1 KARGO_E2E_NATIVE=1 KARGO_E2E_ANDROID=1 cargo test --test e2e
KARGO_E2E=1 KARGO_E2E_GUI=1 cargo test --test e2e compose_hot_reload   # Compose hot reload; opens a window
```

Pushing a tag `v<version>` (equal to the version in `Cargo.toml`) runs `.github/workflows/release.yml`: it builds eight targets, publishes the archives, `SHA256SUMS` and `VERSION` with generated release notes and build provenance attestations, and renders the Homebrew, Scoop, winget and AUR files with `packaging/render.sh`. `website/public/install.sh` and `install.ps1` are the install scripts (try them against a local release directory with `KARGO_RELEASES_URL=file:///...`), and `flake.nix` builds Kargo for Nix. See [Releases](https://enekocm7.github.io/kargo/reference/contributing/#releases).

The JetBrains plugin in `editors/jetbrains` is a Gradle project written in Kotlin, built on [LSP4IJ](https://github.com/redhat-developer/lsp4ij):

```bash
cd editors/jetbrains
./gradlew buildPlugin test                   # the plugin zip in build/distributions
./gradlew runIde                             # an IDE with the plugin installed
```

The Zed extension in `editors/zed` is a Rust crate of its own, compiled to WebAssembly:

```bash
cd editors/zed
cargo test                                   # unit tests, on the host
cargo build --release --target wasm32-wasip2 # what Zed builds; install it with zed: install dev extension
```

The VS Code extension in `editors/vscode` is TypeScript, built with Node.js 22 or newer:

```bash
cd editors/vscode
npm ci
npm test                                     # unit tests
npm run test:integration                     # VS Code with the extension and a new project; needs kargo on PATH
npm run package                              # kargo-vscode.vsix, to install from VS Code
```

The Emacs package in `editors/emacs` is Emacs Lisp, with no dependencies for its tests:

```bash
cd editors/emacs
emacs -Q --batch -L . -l test/kargo-tests.el -f ert-run-tests-batch-and-exit   # needs Emacs 29.1 or newer
```

The Neovim plugin in `editors/neovim` is Lua, with no dependencies for its tests:

```bash
cd editors/neovim
nvim --headless -u NONE -l tests/run.lua     # unit tests; needs Neovim 0.11 or newer
```
