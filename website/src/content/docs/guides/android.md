---
title: Android
description: Build, run and test Android apps and libraries from kargo.toml.
---

Android apps are built by the Android Gradle Plugin (AGP), which Kargo drives for you. Kargo generates a complete Gradle project under `target/android` from `kargo.toml` every time you build, so you configure everything in `kargo.toml` and never edit Gradle files.

## Requirements

- An Android SDK. Either let Kargo install one with `[toolchain] android-sdk = "36"` (see [Android SDK](#android-sdk)), or install [Android Studio](https://developer.android.com/studio) and set `ANDROID_HOME` (or `ANDROID_SDK_ROOT`) to its SDK.
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
# target-sdk = 36                        # or compile-sdk, when that is lower
# compile-sdk = 37                       # default: [toolchain] android-sdk, else 37
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

## Android SDK

Kargo can install the Android SDK the project needs, the way it installs a JDK. Name the platform API level under `[toolchain]`:

```toml title="kargo.toml"
[toolchain]
android-sdk = "36"

[target.android]
namespace = "com.example.myapp"   # compile-sdk defaults to 36 now
```

```console
$ kargo toolchain install android-sdk --accept-licenses
...the terms of the Android Software Development Kit License Agreement...
note: accepted the license `android-sdk-license` above for platforms;android-36, build-tools;36.0.0, platform-tools (--accept-licenses)
 Downloading android-platform-36_r02.zip
  Installing Android SDK Platform 36 (platforms;android-36)
 Downloading android-build-tools_r36_linux.zip
  Installing Android SDK Build-Tools 36 (build-tools;36.0.0)
 Downloading android-platform-tools_r37.0.1-linux.zip
  Installing Android SDK Platform-Tools (platform-tools)
   Installed Android SDK platform 36, build-tools 36.0.0 and platform-tools at /home/me/.kargo/toolchains/android-sdk
```

Kargo installs three packages into one SDK directory, `~/.kargo/toolchains/android-sdk`:

- the platform of the API level (`platforms;android-36`; from API 37 on, `platforms;android-37.0`),
- the build-tools version the Android Gradle Plugin in use expects (36.0.0 for AGP 9.x),
- `platform-tools`, which has `adb` for `kargo run`.

It reads the packages from Google's repository manifest on `dl.google.com`, checks every download against the SHA-1 the manifest lists for it, and writes the same layout as `sdkmanager`. The build then points Gradle at this SDK, through `sdk.dir` in the generated `local.properties` and `ANDROID_HOME`, whatever `ANDROID_HOME` says outside Kargo. Several projects share the SDK directory, each with its own platform.

`compile-sdk` defaults to the `android-sdk` version, and if you set both they must be equal:

```console
$ kargo build -t android
error: [target.android] `compile-sdk = 35` differs from `android-sdk = "36"` under [toolchain]; remove `compile-sdk`, which defaults to the [toolchain] version, or set both to the same API level
```

### Licenses

The SDK packages are under Google's license terms, which you have to accept, and Kargo never accepts them for you. The first install shows the terms and asks:

- In a terminal, Kargo asks `Do you accept the license ...? [y/N]`, and installs only on `y`.
- Without a terminal (CI), the install stops and asks you to run `kargo toolchain install android-sdk --accept-licenses`, which shows the terms and accepts them.

The acceptance is recorded in `~/.kargo/toolchains/android-sdk/licenses/`, as `sdkmanager --licenses` does, so later installs of packages under the same terms don't ask again.

### When it is installed

`kargo toolchain install android-sdk` installs it, and so do `kargo toolchain install` (everything the project needs) and `kargo fetch`. If it is still missing, the first Android build installs it, asking for the licenses if they aren't accepted yet; `KARGO_AUTO_INSTALL=0` turns that off. An `--offline` build without it fails and tells you to run `kargo fetch` while online.

In CI, install it in a step of its own before building:

```sh
kargo toolchain install android-sdk --accept-licenses
kargo build -t android --release
```

`kargo toolchain available android-sdk` lists the platforms you can install, `kargo toolchain list` the installed packages, and `kargo toolchain remove android-sdk 36` removes a platform (or `build-tools;36.0.0` another package). See [Toolchains](../toolchains/#android-sdk).

### The system SDK

Without `[toolchain] android-sdk` (or with `android-sdk = "system"`), Kargo uses the SDK you installed: `ANDROID_HOME`, then `ANDROID_SDK_ROOT`, then Android Studio's default location (`%LOCALAPPDATA%\Android\Sdk` on Windows, `~/Library/Android/sdk` on macOS, `~/Android/Sdk` on Linux). Kargo doesn't change that SDK. When it lacks the platform of `compile-sdk` or the build-tools of AGP, and no license is accepted in it (which would let AGP download them itself), Kargo stops before Gradle runs and names them:

```console
$ kargo build -t android
error: the Android SDK at /home/me/Android/Sdk lacks platforms;android-37.0 and build-tools;36.0.0; install with `sdkmanager "platforms;android-37.0" "build-tools;36.0.0"`, or set `android-sdk = "37"` under [toolchain] in kargo.toml to have Kargo manage the SDK
```

The NDK is not managed yet.

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

For `--offline` builds, download Gradle (and the [Android SDK](#android-sdk), when Kargo manages it) beforehand with `kargo fetch` or `kargo toolchain install`. If Gradle isn't installed and Kargo is offline, it falls back to a `gradle` on `PATH` and warns.

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

Android is just another target. Common code in `src/` is shared with JVM, Wasm, JS and Native, and `src-android/` holds `actual` implementations. See [Multiplatform projects](../multiplatform/).

Code can also live in other packages of a [workspace](../workspaces/#android-members): an Android app can depend on path members. An Android library among them (`kind = "library"`) becomes a Gradle subproject of the app's generated build, and a package without `[target.android]` comes in as the JVM jar Kargo builds for it:

```toml title="apps/android-app/kargo.toml"
[dependencies]
core = { path = "../../libs/core" }       # a multiplatform library
ui-kit = { path = "../../libs/ui-kit" }   # an Android library
```

## Opening in Android Studio

You can open `target/android` in Android Studio to use the layout editor or debugger. Don't edit the generated Gradle files there: Kargo rewrites them on every build. Change `kargo.toml` instead.

When Kargo manages Gradle, it also writes a Gradle wrapper into `target/android` (`gradlew`, `gradlew.bat` and `gradle/wrapper/`), pinned to the same Gradle version and checksum. Android Studio uses it when it opens the project, and you can run `./gradlew` in `target/android` yourself.
