---
title: Gradle feature parity
description: What Gradle can do that Kargo does, does differently, plans or won't do, and which Kargo feature replaces each popular Gradle plugin.
---

If you are deciding whether to move off Gradle, the question is *what can't I do?* This page lists Gradle features by area with their status in Kargo. To move a project, see [Migrating from Gradle](../from-gradle/).

| Status | Meaning |
|---|---|
| **Supported** | Kargo does it, in the same way or close to it. |
| **Different** | Kargo covers the need another way. The row says how. |
| **Planned** | Not there yet; the issue number tracks it. |
| **Not planned** | Kargo leaves it out on purpose. The row says why. |
| **Gap** | Not there, and no issue is open yet. |

## Mindset differences

- **One package per `kargo.toml`.** There is no `settings.gradle.kts` plus a script per module. A [workspace](../../guides/workspaces/) lists member packages, and members inherit shared settings and dependencies with `{ workspace = true }`.
- **No scripting language in the build.** `kargo.toml` is data. There is no `build.gradle.kts` to compute things in, no `afterEvaluate`, no `allprojects {}`. Behavior that Gradle gets from a script comes from [frameworks](../../guides/frameworks/), [`[[generate]]` steps](../../guides/code-generation/) or tasks.
- **Tasks are shell commands, plugins are executables.** A task is a command in `[tasks]` that you can run with `kargo <task>` or attach to a hook, and a plugin is a program called `kargo-<name>`. There is no task graph API and no plugin classpath. See [Tasks, hooks and plugins](../../guides/tasks-and-plugins/).
- **Fixed lifecycle and layout.** `build`, `check`, `run`, `test` and `package` always exist, and sources live in `src/`, `tests/` and `resources/` (see [Project layout](../../getting-started/project-layout/)).
- **Android still uses Gradle** underneath, but Kargo writes the Gradle project from your `kargo.toml`.

## Dependencies

| Gradle feature | Status | In Kargo |
|---|---|---|
| Maven repositories, POM inheritance, BOMs, exclusions, ranges, relocations | Supported | [Dependencies](../../guides/dependencies/) |
| Gradle module metadata (Kotlin Multiplatform variants) | Supported | Resolved per target; see [Multiplatform](../../guides/multiplatform/) |
| Conflict resolution (highest wins, `strictly`) | Supported | Highest wins, `force = true` overrides; see [Version conflicts](../../guides/dependencies/#version-conflicts) |
| Platforms (`platform(...)`) | Supported | [BOMs](../../guides/dependencies/#boms-platforms) |
| Dependency constraints in your own build (`constraints {}`) | Different | Declare the module yourself at the version you want, or with `force = true`. Constraints published by libraries are honored |
| Dependency insight (`dependencyInsight`) | Supported | [`kargo why`](../../reference/commands/#kargo-why), `kargo tree --invert` |
| Dependency tree (`dependencies`) | Supported | [`kargo tree`](../../reference/commands/#kargo-tree) |
| Version catalogs (`libs.versions.toml`) | Different | `kargo import` reads them. In a project, share versions with `[workspace.dependencies]` |
| Dependency locking | Supported | `kargo.lock` is always on and pins the SHA-256 of every artifact; see [the lockfile](../../reference/lockfile/) |
| Dependency verification (`verification-metadata.xml`) | Supported | The checksums in `kargo.lock`; `--locked` fails when they would change |
| Dependency updates (Versions plugin) | Supported | [`kargo outdated`](../../reference/commands/#kargo-outdated), `kargo upgrade` |
| Vulnerability scanning (Dependency-Check) | Supported | [`kargo audit`](../../reference/commands/#kargo-audit) |
| Repository content filtering | Supported | `include-groups` and `exclude-groups`; see [Choosing which repository serves a library](../../guides/dependencies/#choosing-which-repository-serves-a-library) |
| SNAPSHOT versions, `mavenLocal()` | Supported | [SNAPSHOT versions](../../guides/dependencies/#snapshot-versions) |
| Dependency substitution (`substitute`, `dependencySubstitution`) | Gap | Use a path dependency or `force = true` on another version |
| Capabilities and capability conflicts | Gap | |
| Dependency exclusions | Supported | `exclude` on a dependency |
| Custom configurations | Not planned | Kargo has fixed scopes (`compile`, `runtime`, `provided`, `processor`, `ksp`), because configurations are build-script code |
| SBOM generation | Gap | `kargo tree --format json` and `kargo metadata` give the data |

## Build

| Gradle feature | Status | In Kargo |
|---|---|---|
| Incremental Kotlin compilation | Supported | [Incremental compilation](../../guides/build-performance/#incremental-compilation) for the JVM |
| Compiler daemon | Supported | [The compiler daemon](../../guides/build-performance/#the-compiler-daemon) |
| Parallel builds | Supported | Always on; `-j` limits workspace members. See [Parallel work](../../guides/build-performance/#parallel-work) |
| Up-to-date checks | Supported | [Skipped work](../../guides/build-performance/#skipped-work) |
| Local build cache | Different | Each compilation fingerprints its inputs and is skipped when unchanged; `target/` is not shared between checkouts |
| Remote build cache | Gap | |
| Configuration cache, build scans | Not planned | There is no configuration phase to cache, since `kargo.toml` is data |
| Composite builds (`includeBuild`) | Different | Path dependencies and workspaces; see [Local packages](../../guides/dependencies/#local-packages) |
| Multi-project builds | Supported | [Workspaces](../../guides/workspaces/) |
| Init scripts, `buildSrc`, convention plugins | Not planned | There is no build scripting. Share settings with `[workspace.*]` tables |
| Custom tasks and task dependencies | Different | [`[tasks]` and hooks](../../guides/tasks-and-plugins/) |
| Custom plugins in Kotlin or Groovy | Different | [Plugins](../../guides/tasks-and-plugins/) are executables named `kargo-<name>` |
| Toolchains (`jvmToolchain`) | Supported | `[toolchain] jdk`; see [Toolchains](../../guides/toolchains/) |
| Wrapper (`gradlew`) | Different | `[toolchain] kargo` sets the minimum Kargo version, and `kargo self update` updates it |
| Offline mode, mirrors, proxies | Supported | [Offline builds and mirrors](../../guides/offline-and-mirrors/), `kargo vendor` |
| Build profiles and environments | Supported | `[profile.<name>]`, `[env]` |
| Code generation | Supported | [`[[generate]]`](../../guides/code-generation/) |
| Watch mode (`--continuous`) | Supported | [`kargo watch`](../../guides/watch/), `run --watch`, `test --watch` |
| Hot reload | Partial | `kargo run --hot` swaps changed classes into a running JVM program or server and recomposes Compose Desktop ([#231](https://github.com/enekocm7/kargo/issues/231), [#220](https://github.com/enekocm7/kargo/issues/220)). Wasm and JS browser apps reload their page with `--watch`. Spring Boot DevTools and Ktor `autoreload` are not wired in |

## Kotlin

| Gradle feature | Status | In Kargo |
|---|---|---|
| JVM, Wasm, JS and Native targets | Supported | [Targets](../../guides/multiplatform/) |
| Android | Different | Generated Gradle project under `target/android`; see [Android](../../guides/android/) |
| Multiplatform source sets | Different | Flat `src/`, `src-jvm/`, `src-wasm/`, `src-js/`, `src-native/`. |
| Compiler arguments, opt-in, language and API version | Supported | [`[kotlin]`](../../reference/manifest/#kotlin) |
| Warnings as errors | Supported | `warnings-as-errors = true` |
| Compiler plugins (serialization, allopen, noarg, compose, ...) | Supported | [`[frameworks]`](../../guides/frameworks/) and `[plugins.<name>]` |
| Annotation processing (kapt) | Different | Java processors with `scope = "processor"`; see [Annotation processors](../../guides/dependencies/#annotation-processors) |
| KSP | Supported | `scope = "ksp"`; see [KSP](../../guides/dependencies/#kotlin-symbol-processing-ksp) |
| Explicit API mode | Different | `compiler-args = ["-Xexplicit-api=strict"]` |
| Binary compatibility validator | Gap | |
| C interop | Supported | [C interop](../../guides/c-interop/) |
| Cross-compiling Native for Apple targets | Different | Needs macOS, as with Gradle; see [Native](../../guides/native/) |
| Shared Android and multiplatform code, XCFramework | Planned | [#229](https://github.com/enekocm7/kargo/issues/229) |

## Testing

| Gradle feature | Status | In Kargo |
|---|---|---|
| Running tests, filters, tags, timeouts | Supported | [Testing](../../guides/testing/) |
| JUnit XML and JSON reports | Supported | [Test reports for CI](../../guides/testing/#test-reports-for-ci) |
| Re-running failed tests | Supported | `kargo test --failed`; see [Rerunning failures](../../guides/testing/#rerunning-failures) |
| Retry plugin (`org.gradle.test-retry`) | Gap | Rerun failures with `--failed` |
| Test suites (`jvm-test-suite`, separate integration tests) | Planned | [#219](https://github.com/enekocm7/kargo/issues/219) |
| Code coverage (JaCoCo, Kover) | Planned | [#218](https://github.com/enekocm7/kargo/issues/218) |
| Test fixtures | Supported | [Test fixtures](../../guides/testing/#test-fixtures) |
| Benchmarks (kotlinx-benchmark, JMH) | Supported | [`kargo bench`](../../guides/benchmarking/) |
| Android local and instrumented tests | Planned | [#221](https://github.com/enekocm7/kargo/issues/221). `kargo android connectedAndroidTest` runs them today |

## Packaging and publishing

| Gradle feature | Status | In Kargo |
|---|---|---|
| Executable and fat jars | Supported | `kargo package`; see [Distributing](../../guides/distributing/) |
| Application distribution (`installDist`, `distZip`) | Planned | [#222](https://github.com/enekocm7/kargo/issues/222) |
| jlink and jpackage | Supported | `--format jlink`, `--format installer` |
| GraalVM native image | Supported | `--format native-image` |
| Shadow relocation, resource merging, `minimize()` | Supported | [`[package.shade]`](../../guides/distributing/#shading) |
| ProGuard and R8 | Supported | [`[package.optimize]`](../../guides/distributing/) |
| Docker and OCI images (Jib) | Planned | [#223](https://github.com/enekocm7/kargo/issues/223) |
| `maven-publish` and `signing` | Supported | [`kargo publish`](../../guides/publishing/) |
| Maven Central Portal | Supported | `kargo publish` |
| Publishing extras: `java-platform` BOMs, in-memory PGP keys, repository presets, relocation POMs, idempotent re-publishing | Supported | [BOMs](../../guides/publishing/#boms), [Signing](../../guides/publishing/#signing), [Hosted repositories](../../reference/manifest/#hosted-repositories); S3 repositories are a gap ([#224](https://github.com/enekocm7/kargo/issues/224)) |
| Dokka | Supported | [`kargo doc`](../../guides/documentation/) |
| Installing tools from Maven or git | Supported | [`kargo install`](../../guides/installing-tools/) |

## Android

| Gradle feature | Status | In Kargo |
|---|---|---|
| Application and library modules | Supported | [Android](../../guides/android/) |
| Product flavors, build config fields | Supported | [Product flavors](../../guides/android/#product-flavors) |
| Devices and emulators | Supported | `kargo run -t android`, `kargo devices`, `kargo android` |
| Android SDK management | Supported | `kargo toolchain install android-sdk` |
| Any Gradle task | Supported | `kargo android <gradle args>` |
| Lint, App Bundles, baseline profiles, NDK | Planned | [#228](https://github.com/enekocm7/kargo/issues/228) |
| Hand-written Gradle files | Not planned | Kargo generates the Gradle project; put the settings in `kargo.toml` |

## IDE support and CI

| Gradle feature | Status | In Kargo |
|---|---|---|
| IntelliJ IDEA and Android Studio | Supported | The Kargo plugin; see [Editors](../../guides/editors/). Gradle import is not used |
| VS Code, Zed, Neovim, Emacs | Supported | [Editors](../../guides/editors/), `kargo lsp`, `kargo dap` |
| Tooling API | Different | `kargo metadata` and `--message-format json` |
| CI setup | Supported | [`kargo init-ci`](../../guides/ci/), `--locked` |
| Importing an existing build | Supported | [`kargo import`](../../guides/importing/). It reads the scripts without running Gradle ([#279](https://github.com/enekocm7/kargo/issues/279) tracks reading them by running Gradle) |

## Converting a Gradle plugin

| Gradle plugin | In Kargo |
|---|---|
| Shadow (`shadowJar`) | `kargo package` (`--fat` for a library) with [`[package.shade]`](../../guides/distributing/#shading) for `relocate`, `exclude`, `minimize()` and service merging |
| Kover, JaCoCo | [#218](https://github.com/enekocm7/kargo/issues/218). |
| detekt | [`kargo lint`](../../guides/formatting-and-linting/), with `[lint] config` for your `detekt.yml` |
| ktlint | [`kargo fmt`](../../guides/formatting-and-linting/) |
| Spotless | `kargo fmt` for Kotlin. Other languages: a [task](../../guides/tasks-and-plugins/) that calls your formatter |
| Dokka | [`kargo doc`](../../guides/documentation/) |
| Versions (`ben-manes`) | `kargo outdated` and `kargo upgrade` |
| SonarQube | A [task](../../guides/tasks-and-plugins/) that runs the scanner, with JUnit reports from `kargo test --report junit` |
| Jib | [#223](https://github.com/enekocm7/kargo/issues/223). Until then, `kargo package` and a Dockerfile |
| Spring Boot | `[frameworks] spring-boot`; see [Spring Boot](../../guides/spring-boot/) |
| Protobuf | A [`[[generate]]` step](../../guides/code-generation/) with `builtin = "protobuf"` |
| SQLDelight | A [`[[generate]]` step](../../guides/code-generation/) with `run` that calls SQLDelight's command-line generator (there is no built-in step) |
| BuildConfig (gmazoure, buildconfig) | `[env] expose` generates a `BuildConfig`; see [Code generation](../../guides/code-generation/) |
| Dependency-Check | [`kargo audit`](../../reference/commands/#kargo-audit) |
| Application, Distribution | `kargo package`; distributions are [#222](https://github.com/enekocm7/kargo/issues/222) |
| Maven Publish, Signing, Vanniktech | [`kargo publish`](../../guides/publishing/) |
| Compose Multiplatform | `[frameworks] compose`; see [Compose](../../guides/compose/) |

## Keeping this page honest

Each **Planned** row names its issue. When a gap closes or opens, change its row here in the same change. Report a missing row or a wrong status in an issue.
