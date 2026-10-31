---
title: Android
description: Build, run and test Android apps and libraries from kargo.toml.
---

Android apps are built by the Android Gradle Plugin (AGP), which Kargo drives for you. Kargo generates a complete Gradle project under `target/android` from `kargo.toml` every time you build, so you configure everything in `kargo.toml` and never edit Gradle files.

## Requirements

- An Android SDK. Either let Kargo install one with `[toolchain] android-sdk = "36"` (see [Android SDK](#android-sdk)), or install [Android Studio](https://developer.android.com/studio) and set `ANDROID_HOME` (or `ANDROID_SDK_ROOT`) to its SDK.
- Nothing else: Kargo downloads the Gradle version your Android Gradle Plugin needs (see [Gradle](#gradle)).
- For `kargo run`: `adb` (from the SDK platform-tools) and a device or emulator. Kargo lists them, asks which to use, and can create and start emulators: see [Devices and emulators](#devices-and-emulators).

## Create an app

```sh
kargo new myapp -t android               # a classic Android View app
kargo new myapp -t android -w compose    # a Jetpack Compose app
```

```toml title="kargo.toml"
[target.android]
namespace = "com.example.myapp"   # default: <group>.<name> of [package]
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
    ├── assets/
    └── jniLibs/            prebuilt native libraries per ABI (arm64-v8a/libfoo.so)
```

With `-w compose`, you can also put Compose Multiplatform resources in `composeResources/` and read them through the generated `Res` class, as on the desktop: Kargo adds the resources runtime and packs the files into the APK's assets. Code in `src/` that uses `Res` then works on both targets. See [Compose resources](../compose/#resources).

### Network access

The manifest of a new application declares the `INTERNET` permission, so the app can make network calls. Without `<uses-permission android:name="android.permission.INTERNET" />` in `android/AndroidManifest.xml`, every request fails at run time with no hint at the cause. `kargo target add android` writes it too. Add the line yourself to a manifest created before that.

Android 9 (API 28) and later also block plain `http://` traffic. Requests to a development server on your machine fail until the app allows cleartext traffic:

```xml title="android/AndroidManifest.xml"
<application
    android:usesCleartextTraffic="true"
    ...>
```

Remove it again before you ship, or serve the API over `https://`. From the emulator, the host machine is `10.0.2.2`, not `localhost`.

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
kargo run -t android                # install on a device or emulator, launch and show its log
kargo test -t android               # local unit tests (testDebugUnitTest)
kargo doc -t android                # API documentation in target/debug/doc/
```

`kargo test -t android` compiles `tests/` and `tests-android/`, runs them with Gradle and prints one `test ... ok` line per test and a `test result:` summary, like the other targets. A failing test makes it exit with status `101`. If the project has test sources but Gradle runs no tests, Kargo reports an error instead of passing silently.

### Instrumented tests

Tests that need a device or an emulator go in `android-tests/`. Kargo registers the directory as the `androidTest` source set of the generated project, sets AndroidX Test's `AndroidJUnitRunner`, and gives the tests your `[dev-dependencies]` plus `androidx.test:runner`, `androidx.test.ext:junit` and `kotlin-test-junit`.

```kotlin title="android-tests/ExampleInstrumentedTest.kt"
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Test
import kotlin.test.assertEquals

class ExampleInstrumentedTest {
    @Test
    fun usesTheAppContext() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        assertEquals("com.example.droid", context.packageName)
    }
}
```

```sh
kargo test -t android --instrumented                      # on the only device, or pick from a list
kargo test -t android --instrumented -s Pixel_8_API_35    # start this virtual device if it is not running
kargo test -t android --instrumented --headless --stop-emulator   # CI: no window, shut it down afterwards
kargo test -t android --instrumented --report junit       # JUnit XML in target/<profile>/android/test-reports/
```

The device is chosen like [`kargo run -t android`](#devices-and-emulators) chooses it (`-s, --device`, `KARGO_ANDROID_DEVICE`, the only device, or a list). Kargo runs `connected<Variant>AndroidTest` on it, reads the JUnit XML that AGP writes, and prints the same `test ... ok` lines, `test result:` summary and `--message-format json` messages as the local tests. The `filter` argument is passed to the runner, so it names a class (`com.example.ExampleInstrumentedTest`) or a method (`com.example.ExampleInstrumentedTest#usesTheAppContext`). `--no-run` only compiles the tests.

| Option | What it does |
|---|---|
| `--include-tag <annotation>`, `--exclude-tag <annotation>` | Only run the tests with, or skip the tests with, this annotation class (`com.example.Slow`). AndroidJUnitRunner filters by annotation, so JUnit 5 `@Tag` names do not apply |
| `--failed` | Rerun the classes that failed in the last unfiltered, unsharded run |
| `--shard <i>/<n>` | Run shard `i` of `n` (`i` counts from 1), to split the tests over several devices in CI |
| `--orchestrator` | Run each test in its own instrumentation with Android Test Orchestrator, so one crash cannot take the others down |
| `--report junit` | Copy the JUnit XML to `target/<profile>/android/test-reports/` |

[`[test] timeout`](../../reference/manifest/#test) limits the whole run: Kargo stops Gradle when it passes. [`[test] test-timeout`](../../reference/manifest/#test) is passed to the runner as the limit of one test.

Kargo clears the device's log before the run and saves it to `target/android/test-results/logcat.txt` afterwards. When a test failed, it also saves a screenshot of the device's screen to `target/android/test-results/failure.png`, which shows the screen as the run ended and not at the failing test.

`kargo metadata` and `kargo lsp` list `android-tests/` among the Android target's test roots, so editors treat the files as tests.

The device to test on is always named on the command line (`-s`, or `KARGO_ANDROID_DEVICE`); there is no `kargo.toml` key for a test virtual device, and Gradle Managed Devices are not planned for now. In CI, start an emulator with `kargo android emulator start --headless` or use `--headless --stop-emulator` as above.

Without `--instrumented`, `kargo test -t android` runs the local tests and leaves `android-tests/` alone.

### Robolectric

Local tests run on the JVM without an Android framework. For code that needs one, add [Robolectric](https://robolectric.org) to the dev dependencies and annotate the tests with `@RunWith(RobolectricTestRunner::class)`:

```toml title="kargo.toml"
[dev-dependencies]
"org.robolectric:robolectric" = "4.14.1"
```

`kargo doc -t android` documents `src/` and `src-android/` with Dokka, using the classpath Gradle resolves for the debug variant. See [API documentation](../documentation/#android).

The generated project compiles Kotlin with the Kotlin Gradle plugin at your `[kotlin] version`, so Android code is compiled with the same Kotlin version as your other targets rather than the version bundled with AGP.

## Devices and emulators

`kargo run -t android` needs a device to install on: a phone with USB debugging, or an emulator (a virtual device, AVD). Kargo lists them, picks one with you, starts and creates emulators when you say so, and passes `-s <serial>` to every `adb` call and `ANDROID_SERIAL` to Gradle, so a second device that is attached is never touched.

```console
$ kargo android devices
ID             NAME              TYPE      API   STATE
emulator-5554  Pixel_8_API_35    emulator  35    device
R58M123ABC     samsung SM-S918B  device    34    device
0123ABCD       0123ABCD          device    -     unauthorized
Tablet_API_34  Tablet_API_34     avd       34    stopped
```

The list has what `kargo run` can target: physical devices (over USB or Wi-Fi), running emulators, and virtual devices that are installed but not running (`avd`, state `stopped`). `kargo devices` is the same, and `--format json` prints `{"devices": [...]}` with `id`, `name`, `kind` (`physical`, `emulator` or `avd`), `state`, `api` and `avd` for each.

### Choosing the device

`--device <serial|avd-name>` (`-s`) or `KARGO_ANDROID_DEVICE` names the device. It is a serial from the list, the name of a virtual device, or the `host:port` of a wireless device, which Kargo connects first. A virtual device that is not running is started; naming it is the request to start it.

```sh
kargo run -t android --device R58M123ABC
kargo run -t android --device Pixel_8_API_35 --headless   # starts the virtual device if needed
KARGO_ANDROID_DEVICE=emulator-5554 kargo run -t android
```

Without a device name, Kargo decides:

- **One device is ready**: it is used.
- **Several are ready**: you pick one from a list with the arrow keys and Enter (`j`/`k`, a digit and Esc work too).
- **None is connected**: Kargo does not start or create an emulator by itself. In a terminal it asks what to do: start one of the installed virtual devices, create a new one, or cancel. Only your answer starts or creates anything.
- **Without a terminal** (CI, a piped stdin, an editor's task that is not a terminal) there is nobody to ask. Kargo stops, lists the devices or virtual devices, and says to pass `--device`.

The choice is made before the build starts, so a question never waits behind a long Gradle run. In `--watch` it is made once.

A device in a state that cannot run apps is named with what to do about it: `unauthorized` (unlock the device and accept the "Allow USB debugging?" prompt), `offline` (reconnect it, or `adb kill-server`) and `no permissions` (udev rules and the `plugdev` group on Linux, the maker's USB driver on Windows).

### Running the app

Kargo runs `install<Variant>` with Gradle, restricted to the chosen device, and starts the launcher activity with `am start -W`, which waits until the activity is up and reports a start that failed. Kargo asks the device for the launcher activity of the app and starts it by name, as Android Studio does, so the usual `<intent-filter>` with the `MAIN` action and the `LAUNCHER` category works without an `android.intent.category.DEFAULT` category. (On a device that cannot say, Kargo starts the app with an implicit `MAIN`/`LAUNCHER` intent, which does need that category.) Then it shows the log of the app (`adb logcat --pid`) on stdout until the app's process ends:

- The exit code is `0` when the app exits, `1` when the log shows it crashed (a `FATAL EXCEPTION` or a fatal signal), and `130` after Ctrl+C.
- Ctrl+C stops the app on the device (`am force-stop`) before Kargo exits.
- `--no-logcat` leaves as soon as the app started. `--logcat` is the default.
- `--watch` installs the app again whenever the project changes, and keeps showing the log of the new process. It chooses the device once, and does not combine with `--no-logcat` or `--stop-emulator`.

```sh
kargo run -t android --device emulator-5554 --stop-emulator   # stop the emulator this run started
```

`--stop-emulator` shuts down an emulator that this run started, once the app is done. An emulator that was already running, or that you started with `kargo android emulator start`, is left alone.

### Virtual devices and the emulator

The emulator and a system image are not among the SDK packages every build needs, so Kargo installs them when you ask, into the SDK it manages (`[toolchain] android-sdk`):

```sh
kargo toolchain install android-sdk 36 --emulator --accept-licenses
```

This installs `emulator`, `cmdline-tools;latest` (which has `avdmanager`) and a system image, `system-images;android-<api>;google_apis;<abi>`. `<abi>` is the one of the host (`x86_64`, or `arm64-v8a` on ARM machines) and `<api>` the platform you name, or `compile-sdk`. Google lists system images in manifests of their own (`sys-img/<tag>/sys-img2-1.xml`), apart from the other packages; Kargo reads them in this order and takes the first image for the API level: `google_apis`, `google_apis_playstore`, then the plain `default` image. The packages are under the same license as the others. The system image is 1 GB or more.

`kargo android avd` and `kargo android emulator` manage the virtual devices:

```sh
kargo android avd create                          # Kargo_API_36: the compile-sdk of the project, else the newest image installed
kargo android avd create Pixel8 --api 35 --hardware pixel_8
kargo android avd list [--format json]
kargo android avd delete Pixel8
kargo android emulator start Pixel8 --headless    # waits until sys.boot_completed
kargo android emulator stop Pixel8                # or: stop emulator-5554
```

`avd create` installs what is missing, after telling you what it downloads and asking (without a terminal it installs only when you passed `--accept-licenses`). The virtual devices of a Kargo-managed SDK live in `~/.kargo/toolchains/android-avd`, because their images are paths into that SDK. With the system SDK they are where Android Studio and `avdmanager` keep them (`ANDROID_AVD_HOME`, else `~/.android/avd`), and Kargo installs nothing there: it names the `sdkmanager` packages to install instead. `avdmanager` needs Java 17 or newer; Kargo uses `JAVA_HOME`, a JDK on `PATH`, or a JDK it installs.

`emulator start` and the `--device` of a virtual device take `--headless` (no window or sound), `--cold-boot` (ignore the saved snapshot), `--wipe-data` (erase the user data first) and `--boot-timeout <seconds>` (default 300). Kargo prints progress while it boots, and an emulator that exits early shows the end of its output. The emulator keeps running after Kargo exits, so the next run starts fast.

### Physical devices

Turn on USB debugging in Developer options, connect the cable and accept the prompt on the device. For wireless debugging (Android 11 and newer), turn on *Wireless debugging* in Developer options:

```sh
kargo android pair 192.168.1.20:40123 123456   # the pairing address and code from "Pair device with pairing code"
kargo android connect 192.168.1.20:37099       # the address on the Wireless debugging screen
kargo android disconnect                       # or one address
```

`pair` without the code lets `adb` ask for it. `kargo run -t android --device 192.168.1.20:37099` connects by itself.

### Instrumented tests and other tasks

Gradle tasks that use a device (`install*`, `uninstall*`, `connected*`, `*AndroidTest`) run on the device chosen the same way:

```sh
kargo android -s R58M123ABC connectedDebugAndroidTest
KARGO_ANDROID_DEVICE=emulator-5554 kargo android connectedCheck
```

### Editors

The run buttons of the editors run `kargo run -t android`, so they ask in a terminal and fail with the list of devices where there is none. Set `KARGO_ANDROID_DEVICE` in the environment of the run configuration or task to pick the device.

`kargo doctor` reports `adb`, the devices and their states, the installed virtual devices, the emulator package and whether the machine accelerates the emulator (KVM, the Windows Hypervisor Platform, Hypervisor.framework).

## Android SDK

Kargo can install the Android SDK the project needs, the way it installs a JDK. Name the platform API level under `[toolchain]`:

```toml title="kargo.toml"
[toolchain]
android-sdk = "36"

[target.android]
namespace = "com.example.myapp"   # compile-sdk defaults to android-sdk (36 here)
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
- the build-tools version the Android Gradle Plugin in use expects (36.0.0 for AGP 9.x), or the `build-tools` you pin under `[target.android]`,
- `platform-tools`, which has `adb` for `kargo run`,

and, when `[target.android.ndk]` names them, the NDK (`ndk;27.2.12479018`) and CMake (`cmake;3.22.1`) of [native code](#native-code).

The emulator, `avdmanager` and a system image come with `--emulator` (see [Virtual devices and the emulator](#virtual-devices-and-the-emulator)).

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

## Manifest placeholders

`AndroidManifest.xml` can use `${name}` placeholders, which Gradle replaces with values from `kargo.toml`. They keep keys and other per-variant values out of the manifest:

```toml title="kargo.toml"
[target.android]
namespace = "com.example.myapp"
manifest-placeholders = { MAPS_API_KEY = "debug-key", APP_LABEL = "My app" }

[target.android.flavors.paid]
dimension = "tier"
manifest-placeholders = { APP_LABEL = "My app Pro" }
```

```xml title="android/AndroidManifest.xml"
<application android:label="${APP_LABEL}">
    <meta-data android:name="com.google.android.geo.API_KEY" android:value="${MAPS_API_KEY}" />
</application>
```

A value can be a string, a boolean or an integer; it is substituted as text. A flavor's value replaces the shared one. Gradle also fills `${applicationId}` itself.

## Resources

`locales` keeps only the resources of the languages you ship, which drops the translations that libraries such as Material and AndroidX bring in:

```toml title="kargo.toml"
[target.android]
namespace = "com.example.myapp"
locales = ["en", "es", "pt-rBR"]
```

Vector drawables, `res/` qualifiers and resource shrinking (`minify` with `shrink-resources`, see [Release builds](#release-builds)) work as in any Android project: put the files in `android/res/`.

## Native code

`[target.android.ndk]` builds C and C++ with the NDK through CMake or ndk-build. The code is yours; Kargo only passes the build to Gradle:

```toml title="kargo.toml"
[target.android]
namespace = "com.example.myapp"

[target.android.ndk]
version = "27.2.12479018"                        # downloaded with the managed SDK
abi-filters = ["arm64-v8a", "x86_64"]            # default: all four ABIs
cmake = "src-android/cpp/CMakeLists.txt"
cmake-version = "3.22.1"
cmake-arguments = ["-DANDROID_STL=c++_shared"]
cpp-flags = ["-std=c++17"]
```

```kotlin title="src-android/Native.kt"
object Native {
    init { System.loadLibrary("native") }
    external fun hello(): String
}
```

Use `ndk-build = "jni/Android.mk"` instead of `cmake` for an ndk-build project. Libraries you built elsewhere go in `android/jniLibs/<abi>/lib<name>.so` and are packaged without any key; `abi-filters` limits which ABIs end up in the APK. With `[toolchain] android-sdk` set, Kargo installs the `version` of the NDK and the `cmake-version` of CMake with the platform, after the license is accepted (see [Android SDK](#android-sdk)); otherwise your SDK needs them, or AGP downloads them when you accepted the licenses.

## Lint

`kargo lint --android` runs the Android Gradle Plugin's lint on the debug variant (the first flavor of each dimension), or with `--release` the release one:

```console
$ kargo lint --android
     Linting myapp v0.1.0 (android, gradle, lint)
      Report /home/me/myapp/target/debug/android/lint/lint-results-debug.html
      Report /home/me/myapp/target/debug/android/lint/lint-results-debug.sarif
      Report /home/me/myapp/target/debug/android/lint/lint-results-debug.xml
error: Android lint found errors; fix them, list them in [target.android.lint], or record them with `kargo lint --android --create-baseline`
```

It exits with code 101 when lint reports errors, so a CI job fails on them. The HTML, XML and SARIF reports are copied to `target/<profile>/android/lint/`; upload the SARIF file to code scanning, or read the XML in other tools. With `--message-format json`, each issue is a `compiler-message` line on stdout (`severity`, `message` with the issue id, `file`, `line` and `column`), like the compilers' diagnostics.

Configure it under `[target.android.lint]`:

```toml title="kargo.toml"
[target.android.lint]
baseline = "lint-baseline.xml"
warnings-as-errors = true
disable = ["MissingTranslation"]
error = ["HardcodedText"]
reports = ["html", "sarif"]
```

To adopt lint on an existing app without fixing everything first, record today's issues as a baseline and commit it. Later runs report only new issues:

```sh
kargo lint --android --create-baseline
```

`lint-baseline.xml` is used when it exists; name another file with `baseline`. A `lint.xml` in the project root (or `config`) is passed to lint, for severity per path. `kargo android lint` runs Gradle's `lint` task with the same settings, and `kargo android lintRelease` or `lintFreeDebug` lint other variants. For the settings of detekt on your Kotlin code, see [Formatting and linting](../formatting-and-linting/).

## Running any Gradle task

`kargo android` passes its arguments to Gradle in the generated project:

```sh
kargo android tasks
kargo android lint
kargo android connectedAndroidTest      # prefer `kargo test -t android --instrumented`
kargo android bundleRelease
```

Tasks that use a device run on the one `--device` (`-s`) or `KARGO_ANDROID_DEVICE` names, or on the one Kargo asks you to pick; see [Devices and emulators](#devices-and-emulators). The same command also manages devices (`kargo android devices`, `avd`, `emulator`, `connect`, `pair` and `disconnect`), so those names are not passed to Gradle.

## Dependencies

AndroidX and other Android libraries come from Google's Maven repository, which Kargo adds automatically:

```toml
[target.android.dependencies]
"androidx.core:core-ktx" = "1.17.0"
"androidx.lifecycle:lifecycle-runtime-ktx" = "2.9.4"
```

### Profile dependencies

`[profile.dev.dependencies]` and `[profile.release.dependencies]` become `debugImplementation` and `releaseImplementation` in the generated Gradle project, so a debug-only library is in debug builds alone. A custom profile does not build for Android yet, and a processor or KSP dependency in a profile is not supported there: put it in `[dependencies]`.

## Android libraries

A package with `kind = "library"` is an Android library: `kargo build -t android` assembles its AAR (classes, resources, `AndroidManifest.xml`, assets, `consumer-proguard-files` and JNI libraries in `android/jniLibs/`) into `target/android/build/outputs/aar/`, and `kargo package -t android` copies it to `target/<profile>/android/`.

```sh
kargo new ui --lib -t android
```

```toml title="ui/kargo.toml"
[package]
name = "ui"
version = "0.1.0"
group = "com.example"

[target.android]
kind = "library"        # namespace defaults to "com.example.ui"
```

The `namespace` is the package of the library's `R` and `BuildConfig` classes. Without one, it is the package's `group` and name, lowercase, with `_` in place of `-` (`com.example.my_lib` for `my-lib`); set `namespace` to choose another. Each module's `R` class holds only its own resources (Kargo sets AGP's `android.nonTransitiveRClass`), so an app reaches a library's resources through the library's namespace:

```kotlin title="app/src-android/Welcome.kt"
import com.example.ui.R as UiR

fun title(context: Context) = context.getString(UiR.string.greeting)
```

An app or another library uses the library as a [path dependency](#sharing-code-with-other-targets). Gradle merges its resources and its manifest into the app's, and its `consumer-proguard-files` into the app's R8 rules. `kargo publish` publishes the AAR of its release build with Gradle module metadata, so Gradle builds of other apps can use it from a Maven repository; see [Publishing Android libraries](../publishing/#android-libraries).

## Sharing code with other targets

Android is just another target. Common code in `src/` is shared with JVM, Wasm, JS and Native, and `src-android/` holds `actual` implementations: Kargo compiles the two directories as fragments of one module, so an `expect` in `src/` matches its `actual` in `src-android/` (and `tests/` with `tests-android/`). Code that only the JVM and Android share goes in a [source set](../multiplatform/#source-sets-for-several-targets) (`[source-sets.jvmAndAndroid] targets = ["jvm", "android"]`, in `src-jvmAndAndroid/`), which Gradle compiles as one more fragment between the two. A shared module with an Android library, a desktop app and an iOS framework is laid out in [A shared module for apps](../multiplatform/#a-shared-module-for-apps). See [Multiplatform projects](../multiplatform/).

Code can also live in other packages of a [workspace](../workspaces/#android-members): an Android app or library can depend on path members. An [Android library](#android-libraries) among them (`kind = "library"`, including a multiplatform package whose `src-android/` is its Android code) becomes a Gradle subproject of the app's generated build, and a package without `[target.android]` comes in as the JVM jar Kargo builds for it:

```toml title="apps/android-app/kargo.toml"
[dependencies]
core = { path = "../../libs/core" }       # a multiplatform library
ui-kit = { path = "../../libs/ui-kit" }   # an Android library
```

## Opening in Android Studio

You can open `target/android` in Android Studio to use the layout editor or debugger. Don't edit the generated Gradle files there: Kargo rewrites them on every build. Change `kargo.toml` instead.

When Kargo manages Gradle, it also writes a Gradle wrapper into `target/android` (`gradlew`, `gradlew.bat` and `gradle/wrapper/`), pinned to the same Gradle version and checksum. Android Studio uses it when it opens the project, and you can run `./gradlew` in `target/android` yourself.
