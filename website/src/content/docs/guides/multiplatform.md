---
title: Multiplatform projects
description: Share Kotlin code between the JVM, Android, WebAssembly, JavaScript and Kotlin/Native with expect/actual and intermediate source sets.
---

A Kargo project can target several platforms at once: `jvm`, `wasm`, `js`, `native` and `android`. Code in `src/` is compiled for all of them; code in `src-<target>/` only for one.

## Create a multiplatform project

```sh
kargo new hello -t jvm,wasm,native
```

```toml title="kargo.toml"
[package]
name = "hello"
version = "0.1.0"
group = "com.example"

[target.jvm]

[target.wasm]
environment = "js"

[target.native]
platforms = ["host"]
kind = "executable"
```

To add a target to an existing project, use `kargo target add`:

```sh
kargo target add wasm native
```

It appends the same `[target.<name>]` sections `kargo new` writes, without touching the rest of `kargo.toml`, and creates an empty `src-<target>/` for platform-specific code. `kargo target remove wasm` disables a target again (its sources stay on disk), and `kargo target list` shows what is enabled. See [`kargo target`](../../reference/commands/#kargo-target).

:::note
A project with **no** `[target]` sections is a JVM project. As soon as you add any `[target.*]` section, only the listed targets are built. `kargo target add` handles this for you by also writing `[target.jvm]`; if you edit `kargo.toml` by hand, add it yourself.
:::

## Sharing code with `expect`/`actual`

Declare what you need in common code with `expect`, then provide an `actual` for every target:

```kotlin title="src/Platform.kt"
package com.example.hello

expect fun platformName(): String

fun greeting(): String = "Hello from ${platformName()}!"

fun main() {
    println(greeting())
}
```

```kotlin title="src-jvm/Platform.kt"
package com.example.hello

actual fun platformName(): String = "the JVM (Java ${System.getProperty("java.version")})"
```

```kotlin title="src-wasm/Platform.kt"
package com.example.hello

actual fun platformName(): String = "WebAssembly"
```

```kotlin title="src-native/Platform.kt"
package com.example.hello

import kotlin.experimental.ExperimentalNativeApi

@OptIn(ExperimentalNativeApi::class)
actual fun platformName(): String = "Kotlin/Native on ${Platform.osFamily}"
```

```kotlin title="src-android/Platform.kt"
package com.example.hello

actual fun platformName(): String = "Android ${android.os.Build.VERSION.RELEASE}"
```

On Android, Kargo has Gradle compile `src/` and `src-android/` as two fragments of one compilation, the way the other targets do, so the `expect` in `src/` finds its `actual` in `src-android/`. The same holds for `tests/` and `tests-android/`. The fragments are only declared when both directories hold Kotlin files.

## Build, run and test each target

```console
$ kargo build                 # all targets
$ kargo run                   # the first of: jvm, native, wasm, js, android
Hello from the JVM (Java 21.0.5)!
$ kargo run -t native
Hello from Kotlin/Native on LINUX!
$ kargo run -t wasm
Hello from WebAssembly!
$ kargo test                  # tests on every target
$ kargo test -t native
$ kargo test --no-fail-fast   # keep testing the other targets when one fails
```

`kargo test` stops at the first target that fails; `--no-fail-fast` (or `[test] fail-fast = false`) tests them all and prints a result per target. `kargo build --keep-going` builds every target and reports each failure. See [Testing every target](../testing/#testing-every-target).

When the host can't build every target, or you rarely want to, list the ones `build`, `check` and `test` use without `-t`. The manifest keeps all its targets, and `-t` still picks any of them:

```toml title="kargo.toml"
[build]
default-targets = ["jvm", "wasm"]
```

In a workspace, write it as `[workspace.build]`. See [`[build]`](../../reference/manifest/#build).

:::tip
`environment = "js"` runs the Wasm target on Node.js. For a web front end use `environment = "browser"`: `kargo run -t wasm` then serves a page instead, and `kargo test -t wasm` runs the tests in a headless browser. See [WebAssembly](../wasm/#browser-apps).
:::

### Target sets

`--target-set` picks targets by where their code runs, so a CI job on a Linux runner doesn't need the Android SDK and a job with an emulator doesn't build everything else:

```sh
kargo test --target-set host     # jvm, wasm, js and native, here
kargo test --target-set device   # android
```

| Set | Targets |
|---|---|
| `host` | JVM, Wasm, JS and Native, the targets that build and run on this machine without a device or an emulator. Native counts when this host's platform (`linux_x64`, `macos_arm64`, ...) is in `platforms` of `[target.native]`; Android and the Apple platforms a Linux or Windows host cannot build are left out |
| `device` | Android. `kargo test -t android` runs its tests as Gradle local unit tests on the JVM, including `tests/` (the `commonTest` of Gradle) next to `tests-android/`; tests that need a device run with `kargo android connectedDebugAndroidTest`, see [Instrumented tests](../android/#instrumented-tests-and-other-tasks) |

`--target-set` works with `build`, `check`, `test` and `bench`, conflicts with `-t`, and fails when no enabled target is in the set. It picks from the enabled targets, so it ignores [`[build] default-targets`](../../reference/manifest/#build).

## JavaScript next to WebAssembly

`[target.js]` compiles the same common code to JavaScript with Kotlin/JS, for consumers that can't use Wasm. Its own code goes in `src-js/` and `tests-js/`, and its own dependencies in `[target.js.dependencies]`:

```toml title="kargo.toml"
[target.jvm]

[target.js]
environment = "node"
```

```kotlin title="src-js/Platform.kt"
package com.example.hello

actual fun platformName(): String = "Kotlin/JS"
```

```console
$ kargo run -t js
Hello from Kotlin/JS!
```

Wasm and JS are separate targets in Kargo, as in Gradle (`wasmJs()` and `js()`), so a project that enables both writes an `actual` for each. See [JavaScript](../js/).

## Source sets for several targets

`src/` is shared by every target and `src-<target>/` belongs to one. For code that a few targets share, like the JVM and Android, or Wasm and JS, declare an intermediate source set. It is the `jvmAndAndroidMain` of Gradle's default hierarchy template:

```toml title="kargo.toml"
[target.jvm]

[target.android]
namespace = "com.example.hello"

[source-sets.jvmAndAndroid]
targets = ["jvm", "android"]
```

Its code goes in `src-jvmAndAndroid/` and its tests in `tests-jvmAndAndroid/` (test fixtures in `test-fixtures-jvmAndAndroid/`). The directory sits between `src/` and `src-<target>/`: the JVM and Android compile it, the other targets don't, and Kargo compiles the three as fragments of one module, so every `expect` finds its `actual` in a directory that is closer to the target:

```kotlin title="src/Clock.kt"
package com.example.hello

expect fun currentTimeMillis(): Long
```

```kotlin title="src-jvmAndAndroid/Clock.kt"
package com.example.hello

actual fun currentTimeMillis(): Long = System.currentTimeMillis()
```

```kotlin title="src-jvmAndAndroid/Files.kt"
package com.example.hello

// `java.io` is on both targets, but not on Wasm or Native.
fun readText(path: String): String = java.io.File(path).readText()
```

A source set can also hold an `expect` that the targets' own directories (`src-jvm/`, `src-android/`) provide an `actual` for. Both directories can use the declarations of `src-jvmAndAndroid/`, and `src-jvmAndAndroid/` sees only common code and the libraries the JVM and Android share.

Sets nest the way Gradle's do. A set whose targets include all of another set's targets is above it, and the lower set sees what the upper one declares:

```toml title="kargo.toml"
[source-sets.web]
targets = ["wasm", "js"]

[source-sets.nonWeb]
targets = ["jvm", "android", "native"]

[source-sets.jvmAndAndroid]
targets = ["jvm", "android"]   # refines nonWeb
```

| Key | Meaning |
|---|---|
| `[source-sets.<name>]` | The name is the directory suffix: `src-<name>/`, `tests-<name>/`. Letters, digits, `-` and `_`, starting with a letter; it can't be `common` or the name of a target |
| `targets` | At least two of `jvm`, `wasm`, `js`, `native` and `android`. No two sets list the same targets. A listed target need not be enabled; sets without an enabled target are ignored |
| `platforms` | For a set of only some Native platforms: Konan platforms or families (`apple`, `ios`, `linux`, ...). See [Sets of some Native platforms](#sets-of-some-native-platforms) |

`kargo metadata` lists each set under `source-sets` of the targets that compile it (with its `platforms` for a set of Native platforms), `kargo doc` documents them as their own source sets, `kargo publish` ships them in the sources jars (as `jvmAndAndroidMain`) and `kargo lsp` analyzes their code for the targets they hold.

### Sets of some Native platforms

Kotlin/Native is one target in Kargo, so `src-native/` is the equivalent of `nativeMain`: every platform in `platforms` of `[target.native]` compiles it. For code that only some of those platforms share, like `appleMain` or `linuxMain`, give the set `platforms` instead of `targets`:

```toml title="kargo.toml"
[target.native]
platforms = ["ios_arm64", "ios_simulator_arm64", "macos_arm64", "linux_x64"]

[source-sets.apple]
platforms = ["apple"]

[source-sets.ios]
platforms = ["ios"]
```

`src-apple/` and `src-ios/` are compiled by the platforms they list and by no others, and they sit *below* `src-native/`, as in Gradle's hierarchy (`nativeMain` → `appleMain` → `iosMain`). So `src-native/` declares an `expect` that `src-apple/` implements, and `src-ios/` sees the declarations of `src-apple/` and of `src-native/`:

```kotlin title="src-native/Device.kt"
package com.example.hello

expect fun deviceName(): String
```

```kotlin title="src-apple/Device.kt"
package com.example.hello

import platform.UIKit.UIDevice

actual fun deviceName(): String = UIDevice.currentDevice.name
```

```kotlin title="src-linux/Device.kt"
package com.example.hello

actual fun deviceName(): String = "Linux"
```

A name in `platforms` is a platform (`ios_arm64`, or Gradle's `iosArm64`) or a family:

| Family | Platforms |
|---|---|
| `apple` | every iOS, macOS, tvOS and watchOS platform |
| `ios`, `macos`, `tvos`, `watchos` | the platforms of that OS |
| `linux`, `mingw` | the Linux and Windows platforms |
| `androidNative` | the Android NDK platforms (`android_arm64`, ...) |

A platform need not be built on this host for its set to exist: a Linux machine skips `src-apple/` with the Apple platforms it cannot build, and the other platforms don't compile it. Sets of platforms nest by the platforms they hold, like those of whole targets, and a set of platforms can't list other targets (`targets` is `["native"]` or left out). Put a `main` function in `src/`, `src-native/` or a set of whole targets: Kargo looks for programs there, not in sets of some platforms.

## A shared module for apps

Most Kotlin Multiplatform projects have a shared module that an Android app, a desktop app and an iOS app consume. In Kargo that is a [workspace](../workspaces/) with the shared package and one package per app:

```text
my-app/
├── kargo.toml                workspace root
├── shared/
│   ├── kargo.toml            [target.jvm], [target.android] kind = "library", [target.native] kind = "framework"
│   ├── src/                  common code
│   ├── src-jvm/  src-android/  src-native/
│   └── src-jvmAndAndroid/    code only the JVM and Android share
├── androidApp/kargo.toml     [target.android], depends on shared
└── desktopApp/kargo.toml     [target.jvm], depends on shared
```

```toml title="shared/kargo.toml"
[package]
name = "shared"
version = "0.1.0"
group = "com.example"

[target.jvm]

[target.android]
kind = "library"
namespace = "com.example.shared"

[target.native]
platforms = ["ios_arm64", "ios_simulator_arm64"]
kind = "framework"

[source-sets.jvmAndAndroid]
targets = ["jvm", "android"]
```

```toml title="androidApp/kargo.toml"
[target.android]
namespace = "com.example.app"

[dependencies]
shared = { path = "../shared" }
```

The Android app depends on `shared`'s AAR (an [Android library](../android/#android-libraries) becomes a Gradle subproject of the app's build, and `shared`'s `src/`, `src-jvmAndAndroid/` and `src-android/` are compiled for it), the desktop app on its JVM jar, and the iOS app on its framework: `kargo build -p shared -t native --xcframework` writes `target/<profile>/native/shared.xcframework` for Xcode (on macOS), and `--swift-package` or `--cocoapods` add a `Package.swift` or a `shared.podspec` next to it for projects that use Swift Package Manager or CocoaPods. See [Android libraries](../android/#android-libraries), [Sharing code with other targets](../android/#sharing-code-with-other-targets) and [Swift Package Manager and CocoaPods](../c-interop/#swift-package-manager-and-cocoapods).

## Dependencies

Multiplatform libraries go in `[dependencies]`; Kargo picks the right artifact for each target using Gradle module metadata. Platform-specific libraries go in `[target.<name>.dependencies]`:

```toml title="kargo.toml"
[dependencies]
"org.jetbrains.kotlinx:kotlinx-datetime" = "0.7.1"
"com.squareup.okio:okio" = "3.10.2"

[target.jvm.dependencies]
"org.slf4j:slf4j-simple" = "2.0.17"
```

Using a JVM-only library in `src/` fails to compile on the other targets. That's your cue to move the code to `src-jvm/`.

## Tips

- Keep business logic in `src/` and put only the thin platform layer in `src-<target>/`.
- Prefer multiplatform libraries ([klibs.io](https://klibs.io) lists them) over `java.*` APIs in common code. For example, use `kotlinx-datetime` instead of `java.time`, and `okio` or `kotlinx-io` for files.
- Tests in `tests/` run on every target, which is a cheap way to catch platform differences.
- `kargo doc` documents `src/` as common code and shows which platforms each declaration is available on. See [API documentation](../documentation/).
- In an editor, `kargo lsp` treats `src/` and each `src-<target>/` as separate source sets, so `expect` and `actual` declarations link up and common code only sees common code and the libraries every target has. [Kargo's own Kotlin server](../kotlin-server/#how-it-reads-your-project) reads the code of every target; with JetBrains' Kotlin LSP, common code is analyzed as JVM code and Wasm, JS and Native code gets no Kotlin code intelligence yet. See [Multiplatform projects](../editors/#multiplatform-projects).
- `kargo publish` publishes a multiplatform library the way the Kotlin Gradle Plugin does, with one module per platform, so Gradle and Kargo projects pick the right artifact. See [Publishing a library](../publishing/#multiplatform-libraries).
