---
title: Android
description: Build, run and test Android apps and libraries from kargo.toml.
---

Android apps are built by the Android Gradle Plugin (AGP), which Kargo drives for you. Kargo generates a complete Gradle project under `target/android` from `kargo.toml` every time you build, so you configure everything in `kargo.toml` and never edit Gradle files.

## Requirements

- The [Android SDK](https://developer.android.com/studio). Set `ANDROID_HOME` (or `ANDROID_SDK_ROOT`) to its location.
- [Gradle](https://gradle.org/install) on your `PATH`, or a `gradlew` wrapper in the project root (the wrapper wins if present).
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

## Build, run, test

```sh
kargo check -t android --tests      # compile sources and unit tests without packaging an APK
kargo build -t android              # debug APK in target/android/build/outputs/apk/debug/
kargo build -t android --release    # release APK (or AAR for libraries)
kargo run -t android                # install on the connected device and launch
kargo test -t android               # local unit tests (testDebugUnitTest)
```

`kargo test -t android` compiles `tests/` and `tests-android/`, runs them with Gradle and prints one `test ... ok` line per test and a `test result:` summary, like the other targets. A failing test makes it exit with status `101`. If the project has test sources but Gradle runs no tests, Kargo reports an error instead of passing silently.

The generated project compiles Kotlin with the Kotlin Gradle plugin at your `[kotlin] version`, so Android code is compiled with the same Kotlin version as your other targets rather than the version bundled with AGP.

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

## Sharing code with other targets

Android is just another target. Common code in `src/` is shared with JVM, Wasm and Native, and `src-android/` holds `actual` implementations. See [Multiplatform projects](../multiplatform/).

## Opening in Android Studio

You can open `target/android` in Android Studio to use the layout editor or debugger. Don't edit the generated Gradle files there: Kargo rewrites them on every build. Change `kargo.toml` instead.
