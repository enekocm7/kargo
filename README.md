# Kargo

[![Crates.io](https://img.shields.io/crates/v/kargo-cli.svg)](https://crates.io/crates/kargo-cli)
[![Release](https://github.com/enekocm7/kargo/actions/workflows/release.yml/badge.svg)](https://github.com/enekocm7/kargo/actions/workflows/release.yml)
[![Docs](https://github.com/enekocm7/kargo/actions/workflows/website.yml/badge.svg)](https://enekocm7.github.io/kargo/)
[![License](https://img.shields.io/github/license/enekocm7/kargo)](LICENSE)

**Kargo is Cargo for Kotlin.** One `kargo.toml` and one CLI create, build, run, test and publish Kotlin projects for the **JVM**, **WebAssembly**, **JavaScript**, **Kotlin/Native** and **Android**.

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

No Gradle scripts, no plugins to wire up. If you have used Cargo, you already know how it works.

## Install

```sh
curl -fsSL https://enekocm7.github.io/kargo/install.sh | sh     # Linux and macOS
```

```powershell
irm https://enekocm7.github.io/kargo/install.ps1 | iex          # Windows
```

You don't need Rust or a JDK: Kargo downloads a JDK when you have none. Nix, `cargo binstall`, `cargo install` and the other options are in the [installation guide](https://enekocm7.github.io/kargo/getting-started/installation/). Update later with `kargo self update`.

## Why Kargo

- **No Gradle for JVM, Wasm, JS and Native.** Kargo drives the Kotlin compilers directly, and downloads them (and a JDK) for you.
- **Real Maven resolution.** POM inheritance, BOMs, exclusions, version ranges and Gradle module metadata, so Kotlin Multiplatform libraries resolve to the right artifact for each target.
- **Reproducible by default.** `kargo.lock` pins every resolved version and the SHA-256 of every artifact and tool.
- **Fast incremental builds.** Unchanged targets are skipped, and changed Kotlin/JVM files recompile in a warm compiler daemon, often in well under a second.
- **Android without Gradle files.** Kargo generates the Gradle project from `kargo.toml` and drives the Android Gradle Plugin for you.
- **Familiar UX.** Cargo-style output, `kargo add`, `kargo tree`, `kargo why`, workspaces, tasks and `kargo-<name>` plugins.

## A `kargo.toml`

```toml
[package]
name = "hello"
version = "0.1.0"
group = "com.example"

[kotlin]
version = "2.4.21"

[target.jvm]                # no [target] at all means a JVM project
[target.wasm]               # add more targets with `kargo target add`

[frameworks]
serialization = true
coroutines = true

[dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
```

Every key is in the [manifest reference](https://enekocm7.github.io/kargo/reference/manifest/).

## Targets and frameworks

| Target | What you get |
|---|---|
| [JVM](https://enekocm7.github.io/kargo/getting-started/quick-start/) | Kotlin and Java sources, fat jars, zip/tar distributions, R8, jlink images, GraalVM native images and installers |
| [WebAssembly](https://enekocm7.github.io/kargo/guides/wasm/) | Node.js, WASI and browser apps (bundled with bun or Rollup), or npm libraries |
| [JavaScript](https://enekocm7.github.io/kargo/guides/js/) | Kotlin/JS for Node.js and browsers, ES, CommonJS or UMD modules with TypeScript declarations |
| [Kotlin/Native](https://enekocm7.github.io/kargo/guides/native/) | Executables and libraries for Linux, macOS, Windows and iOS, with C interop and XCFrameworks |
| [Android](https://enekocm7.github.io/kargo/guides/android/) | Apps and libraries through the Android Gradle Plugin, with flavors, signing, devices and emulators |

Built-in [frameworks](https://enekocm7.github.io/kargo/guides/frameworks/) add their libraries, compiler plugins and flags with one line: `serialization`, `coroutines`, [`compose`](https://enekocm7.github.io/kargo/guides/compose/) (desktop, web, iOS and Android), [`ktor`](https://enekocm7.github.io/kargo/guides/ktor/), [`spring-boot`](https://enekocm7.github.io/kargo/guides/spring-boot/), `lombok` and `benchmark`. Start from one with `kargo new my-app --with ktor`.

## Everyday commands

| Command | What it does |
|---|---|
| `kargo new <dir>` | Create a project (`--lib`, `-t jvm,wasm,js,native,android`, `--with compose,ktor,...`, or `--template <git URL or path>` for your own) |
| `kargo build` | Compile every target (`--release` for an optimized build) |
| `kargo run` | Build and run the program, or serve a browser app (`--watch` restarts it on changes) |
| `kargo test` | Compile and run the tests of every target (`--coverage` measures JVM code coverage) |
| `kargo add <group:artifact>` | Add a dependency, at its latest version unless you give one |
| `kargo package` | Build a fat jar, a runtime image, a native image, an installer or Android bundles |
| `kargo publish` | Publish a library to Maven Central, a Maven repository or npm |
| `kargo import` | Convert a Maven, Gradle or Kotlin Toolchain project (`--keep-layout` keeps the sources where they are) |

There are many more: `check`, `coverage`, `fmt`, `lint`, `doc`, `tree`, `why`, `audit`, `bench`, `script` for single files, and others. See the [commands reference](https://enekocm7.github.io/kargo/reference/commands/), or run `kargo --list`.

## Learn more

- [Documentation](https://enekocm7.github.io/kargo/): start with the [quick start](https://enekocm7.github.io/kargo/getting-started/quick-start/).
- [Examples](examples/): complete projects for the JVM, Ktor, Spring Boot, Compose Multiplatform, Android, Wasm, JS and Native that you can copy and run.
- [Guides](https://enekocm7.github.io/kargo/guides/dependencies/): dependencies, workspaces, testing, toolchains, CI, offline builds and more.
- [Editor support](https://enekocm7.github.io/kargo/guides/editors/): plugins for IntelliJ IDEA and Android Studio, VS Code, Zed, Neovim and Emacs, and `kargo lsp` for every other editor.
- Coming from Gradle, Maven or the [Kotlin Toolchain](https://kotlin-toolchain.org/)? `kargo import` converts your project. See [migrating from Gradle](https://enekocm7.github.io/kargo/migration/from-gradle/), [from the Kotlin Toolchain](https://enekocm7.github.io/kargo/migration/from-kotlin-toolchain/) and the [feature parity table](https://enekocm7.github.io/kargo/migration/gradle-feature-parity/).

## Contributing

Issues and pull requests are welcome. Found a bug? Run `kargo report` in the project: it writes a redacted report (versions, `kargo.toml`, the last failed build and `kargo doctor`) that you can paste into an issue, and `kargo report --open` opens a prefilled one. Nothing is uploaded.

Kargo is written in Rust:

```sh
cargo build
cargo test --workspace
```

The [contributing guide](https://enekocm7.github.io/kargo/reference/contributing/) covers the architecture, the end-to-end tests, the editor plugins, the website and releases.
