---
title: Android
description: Build, run and test Android apps and libraries from kargo.toml.
---

Android apps are built by the Android Gradle Plugin (AGP), which Kargo drives for you. Kargo generates a complete Gradle project under `target/android` from `kargo.toml` every time you build, so you configure everything in `kargo.toml` and never edit Gradle files.

## Requirements

- The [Android SDK](https://developer.android.com/studio). Set `ANDROID_HOME` (or `ANDROID_SDK_ROOT`) to its location.
- Nothing else: Kargo downloads the Gradle version your Android Gradle Plugin needs (see [Gradle](#gradle)).
- For `kargo run`: `adb` (from the SDK platform-tools) and a connected device or running emulator.

## Create an app

```sh
kargo new myapp -t android               # a classic Android View app
kargo new myapp -t android -w compose    # a Jetpack Compose app
```

```toml title="kargo.toml"
[target.android]
namespace = "com.example.myapp"
kind = "application"        # or "library"
min-sdk = 24
# target-sdk = 36
# compile-sdk = 37
# application-id = "com.example.myapp"   # defaults to namespace
# version-code = 1
# version-name = "0.1.0"
# agp-version = "..."                    # pin the Android Gradle Plugin version
# gradle-version = "..."                 # pin the Gradle version (default: the one AGP needs)
```

```text
myapp/
├── kargo.toml
├── src/                    shared Kotlin code
├── src-android/            MainActivity.kt and other Android-only code
└── android/
    ├── AndroidManifest.xml
    ├── res/                layouts, strings, drawables
    └── assets/
```

With `-w compose`, you can also put Compose Multiplatform resources in `composeResources/` and read them through the generated `Res` class, as on the desktop: Kargo adds the resources runtime and packs the files into the APK's assets. Code in `src/` that uses `Res` then works on both targets. See [Compose resources](../compose/#resources).

### Add Android to an existing project

```sh
kargo target add android          # an application (or a library when the sources have no `main`)
kargo target add android --lib    # an Android library
```

This writes `[target.android]` with the namespace `<group>.<name>`, `min-sdk = 24` and the `kind`, then creates `android/AndroidManifest.xml` and, for an application, `src-android/MainActivity.kt`. Existing files are never overwritten. The activity calls `greeting("Android")` when `src/` declares that function in the same package (as `kargo new` projects do), and shows a fixed text otherwise. Build it with `kargo build --target android`. See [`kargo target`](../../reference/commands/#kargo-target).

## Build, run, test

```sh
kargo check -t android --tests      # compile sources and unit tests without packaging an APK
kargo build -t android              # debug APK in target/android/build/outputs/apk/debug/
kargo build -t android --release    # release APK (or AAR for libraries)
kargo package -t android --release  # release App Bundle and APK in target/release/android/
kargo run -t android                # install on the connected device and launch
kargo test -t android               # local unit tests (testDebugUnitTest)
kargo doc -t android                # API documentation in target/debug/doc/
```

`kargo test -t android` compiles `tests/` and `tests-android/`, runs them with Gradle and prints one `test ... ok` line per test and a `test result:` summary, like the other targets. A failing test makes it exit with status `101`. If the project has test sources but Gradle runs no tests, Kargo reports an error instead of passing silently.

`kargo doc -t android` documents `src/` and `src-android/` with Dokka, using the classpath Gradle resolves for the debug variant. See [API documentation](../documentation/#android).

The generated project compiles Kotlin with the Kotlin Gradle plugin at your `[kotlin] version`, so Android code is compiled with the same Kotlin version as your other targets rather than the version bundled with AGP.

## Gradle

You don't install Gradle. The first Android build downloads the Gradle version the Android Gradle Plugin (AGP) in use requires from `services.gradle.org`, verifies the SHA-256 Gradle publishes for it, and installs it under `~/.kargo/toolchains/gradle/<version>`. With the default AGP 9.4.1 that is Gradle 9.6.0. Gradle runs on the project's JDK (`[toolchain] jdk`), which must be JDK 17 or newer for AGP 8 and later.

Behind a company mirror, the generated `settings.gradle.kts` uses the [`[mirrors]`](../offline-and-mirrors/#repository-mirrors) of `~/.kargo/config.toml` in place of `google()`, `mavenCentral()`, `gradlePluginPortal()` and your `[repositories]`, and `[toolchain-mirrors]` applies to the Gradle download.

Kargo picks the Gradle that runs the build in this order:

1. A Gradle wrapper (`gradlew`) in the project root, if there is one.
2. With `--system-gradle` (or `KARGO_SYSTEM_GRADLE=1`), the `gradle` on your `PATH`.
3. A Kargo-managed Gradle: `gradle-version` under `[target.android]` if set, otherwise the version AGP requires.

`kargo toolchain which` shows which one a project uses, and `kargo build -v` prints it:

```console
$ kargo toolchain which
jdk:           system (JDK 21 at /usr/lib/jvm/jdk-21)
kotlin:        2.4.21
gradle:        9.6.0 (default for AGP 9.4.1) at /home/me/.kargo/toolchains/gradle/9.6.0
```

To use a newer Gradle than AGP requires, pin it:

```toml title="kargo.toml"
[target.android]
namespace = "com.example.myapp"
gradle-version = "9.7"
```

Kargo checks `agp-version` and `gradle-version` against AGP's compatibility table before it runs anything, and stops with the versions that would work:

```console
$ kargo build -t android
error: Gradle 9.1.0 is too old for Android Gradle Plugin 9.4.1, which needs Gradle 9.6.0 or newer; set `gradle-version = "9.6.0"` under [target.android] (or remove it to use 9.6.0), or pick an older `agp-version`
```

For `--offline` builds, download Gradle beforehand with `kargo fetch` or `kargo toolchain install gradle`. If Gradle isn't installed and Kargo is offline, it falls back to a `gradle` on `PATH` and warns.

Gradle keeps its daemon and caches in `~/.gradle` (`GRADLE_USER_HOME`), shared with any other Gradle builds on the machine, so later builds start faster.

## Release builds

A release build that you can install or upload to Google Play needs a signing key. Create a keystore once and keep it out of version control:

```sh
keytool -genkeypair -v -keystore release.jks -alias upload -keyalg RSA -keysize 2048 -validity 10000
```

Then point `kargo.toml` at it. The passwords stay in environment variables, like the credentials of [`[repositories]`](../../reference/manifest/#repositories):

```toml title="kargo.toml"
[target.android]
namespace = "com.example.myapp"
minify = true                                   # R8: shrink, obfuscate and optimize
proguard-files = ["android/proguard-rules.pro"] # your keep rules

[target.android.signing.release]
store-file = "release.jks"
store-password-env = "KS_PASS"
key-alias = "upload"
key-password-env = "KEY_PASS"                   # default: store-password-env
```

```console
$ export KS_PASS=... KEY_PASS=...
$ kargo package -t android --release
   Compiling myapp v0.1.0 (android, gradle)
    Packaged /home/me/myapp/target/release/android/myapp-release.aab
    Packaged /home/me/myapp/target/release/android/myapp-release.apk
```

`kargo package -t android` runs `bundleRelease` and `assembleRelease` (`bundleDebug` and `assembleDebug` without `--release`) and copies the App Bundles and APKs to `target/<profile>/android/`, App Bundles first. A library gets its AAR. Upload the `.aab` to Google Play; install the `.apk` directly. `kargo build -t android --release` signs the APK in place, and `kargo run -t android --release` installs the signed app.

Before Gradle runs, Kargo checks that the keystore exists and that the password variables are set, so a missing secret fails fast with the variable's name. A release build of an app without `[target.android.signing.release]` is unsigned: Kargo warns about it, and `kargo run --release` refuses to install it. `[target.android.signing.debug]` replaces the debug keystore AGP generates, for example to share one across a team.

With `minify = true`, release builds are shrunk by R8 with AGP's `proguard-android-optimize.txt` rules and your `proguard-files`, and unused resources are removed (`shrink-resources = false` keeps them). A library can ship rules for the apps that use it with `consumer-proguard-files`.

### Build config fields

`build-config` adds constants to the generated `BuildConfig` class (`<namespace>.BuildConfig`). The TOML type picks the Java type:

```toml title="kargo.toml"
[target.android]
namespace = "com.example.myapp"
build-config = { API_URL = "https://api.example.com", LOGGING = false, RETRIES = 3 }
```

```kotlin title="src-android/Api.kt"
val client = ApiClient(BuildConfig.API_URL, retries = BuildConfig.RETRIES)
```

## Product flavors

Flavors build several versions of one app, such as a free and a paid one:

```toml title="kargo.toml"
[target.android.flavors.free]
dimension = "tier"
application-id-suffix = ".free"
build-config = { ADS = true }

[target.android.flavors.paid]
dimension = "tier"
build-config = { ADS = false }
```

`kargo build` and `kargo package` build every flavor. `kargo run` installs one: the first flavor of each dimension by name, or the one you pick with `--flavor`:

```sh
kargo run -t android --flavor paid
```

`kargo check`, `kargo test` and `kargo doc` use the first flavor of each dimension. With more than one dimension, list them in priority order in `flavor-dimensions`, and pass one `--flavor` per dimension you want to choose:

```toml title="kargo.toml"
[target.android]
namespace = "com.example.myapp"
flavor-dimensions = ["tier", "env"]
```

Sources and resources are shared by all flavors; to build something only in one flavor, read a `build-config` field. Any other Gradle task of a flavor runs through `kargo android`, for example `kargo android assembleFreeRelease`.

## Running any Gradle task

`kargo android` passes its arguments to Gradle in the generated project:

```sh
kargo android tasks
kargo android lint
kargo android connectedAndroidTest
kargo android bundleRelease
```

## Dependencies

AndroidX and other Android libraries come from Google's Maven repository, which Kargo adds automatically:

```toml
[target.android.dependencies]
"androidx.core:core-ktx" = "1.17.0"
"androidx.lifecycle:lifecycle-runtime-ktx" = "2.9.4"
```

### Profile dependencies

`[profile.dev.dependencies]` and `[profile.release.dependencies]` become `debugImplementation` and `releaseImplementation` in the generated Gradle project, so a debug-only library is in debug builds alone. A custom profile does not build for Android yet, and a processor or KSP dependency in a profile is not supported there: put it in `[dependencies]`.

## Sharing code with other targets

Android is just another target. Common code in `src/` is shared with JVM, Wasm and Native, and `src-android/` holds `actual` implementations. See [Multiplatform projects](../multiplatform/).

Code can also live in other packages of a [workspace](../workspaces/#android-members): an Android app can depend on path members. An Android library among them (`kind = "library"`) becomes a Gradle subproject of the app's generated build, and a package without `[target.android]` comes in as the JVM jar Kargo builds for it:

```toml title="apps/android-app/kargo.toml"
[dependencies]
core = { path = "../../libs/core" }       # a multiplatform library
ui-kit = { path = "../../libs/ui-kit" }   # an Android library
```

## Opening in Android Studio

You can open `target/android` in Android Studio to use the layout editor or debugger. Don't edit the generated Gradle files there: Kargo rewrites them on every build. Change `kargo.toml` instead.

When Kargo manages Gradle, it also writes a Gradle wrapper into `target/android` (`gradlew`, `gradlew.bat` and `gradle/wrapper/`), pinned to the same Gradle version and checksum. Android Studio uses it when it opens the project, and you can run `./gradlew` in `target/android` yourself.
