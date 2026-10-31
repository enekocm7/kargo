---
title: Compose
description: Build user interfaces with Compose Multiplatform for the desktop, the web, iOS and Android, with Compose resources, hot reload, previews and installers.
---

[Compose Multiplatform](https://www.jetbrains.com/compose-multiplatform/) lets you build user interfaces in Kotlin for the desktop, the browser, iOS and Android, from shared code (and, on Kotlin/JS, [Compose HTML](#compose-html-dom-elements), which renders DOM elements). Kargo sets up the compiler plugin, the Compose libraries of each target, Material 3 and Compose resources. It reloads a running desktop app when you save, renders `@Preview` functions to images, and packages desktop apps as installers.

## Create the project

```sh
kargo new counter -w compose
cd counter
kargo run
```

A window opens with a button that counts clicks:

```kotlin title="src/Main.kt"
package com.example.counter

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application

fun greeting(name: String) = "Hello, $name!"

fun main() =
    application {
        Window(onCloseRequest = ::exitApplication, title = "counter") {
            MaterialTheme {
                var clicks by remember { mutableStateOf(0) }
                Column(
                    modifier = Modifier.fillMaxSize(),
                    verticalArrangement = Arrangement.Center,
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Text(greeting("Compose"))
                    Button(onClick = { clicks++ }) { Text("Clicked $clicks times") }
                }
            }
        }
    }
```

## One UI for every target

`--with compose-multiplatform` creates a project whose UI lives in `src/App.kt` and runs on the desktop and in the browser:

```sh
kargo new counter -w compose-multiplatform
cd counter
kargo run              # the desktop app
kargo run -t wasm      # the same UI in the browser, at http://127.0.0.1:8080/
```

```text
counter/
  kargo.toml           [target.jvm], [target.wasm] environment = "browser", [frameworks] compose = true
  src/App.kt           @Composable fun App(), shared by every target
  src-jvm/Main.kt      the desktop window, and a @Preview of App()
  src-wasm/Main.kt     ComposeViewport(document.body!!) { App() }
  tests/GreetingTest.kt
```

Add `-t android` for an Android app whose activity shows `App()`, and `-t native` for an iOS framework (see [iOS](#ios)). Each target keeps its entry point in its own `src-<target>/` directory, and `src/` holds what they share. See [Multiplatform projects](../multiplatform/) for how the directories combine.

## How state works

`remember { mutableStateOf(0) }` creates a value that survives redraws. When it changes, Compose redraws the parts of the UI that read it. Split your UI into `@Composable` functions:

```kotlin
import androidx.compose.runtime.Composable

@Composable
fun TodoList(items: List<String>, onRemove: (String) -> Unit) {
    Column {
        for (item in items) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(item, Modifier.weight(1f))
                TextButton(onClick = { onRemove(item) }) { Text("Remove") }
            }
        }
    }
}
```

## Coroutines in the UI

Enable `coroutines` too and Kargo adds `kotlinx-coroutines-swing`, which provides the main (UI) dispatcher on desktop:

```toml
[frameworks]
compose = true
coroutines = true
```

## Resources

Put images, fonts, strings and other files in `composeResources/`, next to `src/`. Kargo generates the `Res` class that Compose Multiplatform code uses, the same one the Compose Gradle plugin generates, and puts the files on the classpath:

```text
composeResources/
├── drawable/logo.png          Res.drawable.logo
├── drawable-dark/logo.png     the same image, in dark mode
├── font/Inter.ttf             Res.font.Inter
├── values/strings.xml         Res.string.*, Res.array.*, Res.plurals.*
├── values-de/strings.xml      the German strings
└── files/data.json            Res.readBytes("files/data.json")
```

```xml title="composeResources/values/strings.xml"
<resources>
    <string name="app_name">Notes</string>
    <string name="greeting">Hello, %1$s!</string>
    <string-array name="colors">
        <item>Red</item>
        <item>Green</item>
    </string-array>
    <plurals name="notes">
        <item quantity="one">%1$d note</item>
        <item quantity="other">%1$d notes</item>
    </plurals>
</resources>
```

```kotlin title="src/Main.kt"
import com.example.notes.generated.resources.*
import org.jetbrains.compose.resources.painterResource
import org.jetbrains.compose.resources.pluralStringResource
import org.jetbrains.compose.resources.stringResource

@Composable
fun Header(count: Int) {
    Image(painterResource(Res.drawable.logo), contentDescription = null)
    Text(stringResource(Res.string.greeting, "Kargo"))
    Text(pluralStringResource(Res.plurals.notes, count, count))
}
```

The rules follow Compose Multiplatform's:

- A resource's name is its file name without the extension, with `-` written as `_` (`compose-logo.png` is `Res.drawable.compose_logo`). Strings take the `name` attribute.
- Directory qualifiers pick a variant: a language (`values-de`), a language and region (`values-de-rAT`), `light` or `dark`, and a density (`drawable-xxhdpi`). They combine, as in `drawable-de-dark`.
- `files/` holds anything else, read with `Res.readBytes(path)` or `Res.getUri(path)`. It takes no qualifiers.
- String values support Android's escapes: `\n`, `\t`, `\'`, `\"`, `\@` and `\uXXXX`.

`Res` lives in `<group>.<name>.generated.resources` (lower case, with `-` and `_` dropped) and is `internal`. Both are configurable:

```toml title="kargo.toml"
[frameworks]
compose = { resources-package = "com.example.notes.res", public-resources = true }
```

Kargo adds the resources runtime (`org.jetbrains.compose.components:components-resources`) once `composeResources/` exists, writes `Res.kt` into `target/<profile>/<target>/generated/compose-resources/` (`target/<profile>/wasm-generated/compose-resources/` on Wasm) on every build and rebuilds when a resource changes, also under `kargo run --watch`. `Res` belongs to the code every target shares, so `src/` can use it. The same directory works for the [Android target](../android/), where the files become assets of the APK, for the [web](#compose-for-the-web), where they are served next to the page, and for [iOS](#ios).

## Compiler options

The Compose compiler takes the options its Gradle plugin's `composeCompiler { }` block sets, as keys of `[frameworks.compose]`:

```toml title="kargo.toml"
[frameworks]
compose = { stability-config = "compose-stability.conf", metrics = true, reports = true }
```

```txt title="compose-stability.conf"
// Classes the compiler should treat as stable.
java.time.*
com.example.notes.model.Note
```

| Key | Default | What it does |
| --- | --- | --- |
| `stability-config` | none | One or more [stability configuration files](https://developer.android.com/develop/ui/compose/performance/stability/fix#configuration-file), relative to the package. The build fails if one is missing |
| `strong-skipping` | `true` | Strong skipping mode. `false` makes composables with unstable parameters non-skippable again |
| `feature-flags` | `[]` | Compose compiler feature flags: `StrongSkipping`, `IntrinsicRemember`, `OptimizeNonSkippingGroups` and `PausableComposition`. Prefix one with `-` to disable it, as in `"-IntrinsicRemember"` |
| `metrics` | `false` | Write the compiler's metrics (`<module>-module.json`) to `target/compose/<target>/metrics/` |
| `reports` | `false` | Write the compiler's stability reports (`<module>-classes.txt`, `<module>-composables.txt`, `<module>-composables.csv`) to `target/compose/<target>/reports/` |

`<target>` is `jvm` or `android`; in a workspace the directories are `target/compose/<member>/<target>/`. The options apply to both targets: on the JVM Kargo passes them to the compiler plugin, on Android it adds them to the generated Gradle project. Changing them rebuilds the package.

### Kotlin and Compose versions

The Compose compiler ships with Kotlin, so its version is always `[kotlin] version`: there is no separate compiler version to match. The runtime (`version`, default 1.12.1) must be a Compose Multiplatform release that supports that Kotlin version, and Kargo checks the pair before it builds, on every target (the JVM, Wasm, JS, Native and Android):

| Kotlin | Compose Multiplatform | Result |
|---|---|---|
| older than 2.0 | any | Error: the Compose compiler plugin is part of Kotlin only from 2.0 on |
| 2.0 or newer | older than 1.6.10 | Error: 1.6.10 is the first release for the Kotlin 2.0 compiler plugin |
| 2.0.x | 1.8.0 or newer, which includes the default | Error: 1.8.0 moved the UI framework to K2 and needs Kotlin 2.1.0 or newer. Raise `[kotlin] version`, or pin an older release (below 1.8.0) with `version` |
| older than 2.2.20 | 1.9.0 or newer, with `[target.wasm]` or `[target.native]` | [Warning](../../reference/warnings/#compose-for-ios-or-the-web-on-an-older-kotlin): JetBrains recommends Kotlin 2.2.20 for iOS and the web |
| 2.1.0 or newer | 1.6.10 or newer | Fine |

A pre-release such as `1.8.0-beta01` counts as the release it leads to. Libraries built on Compose Multiplatform must be compiled against at least the Kotlin version of the project, so check them when you raise the Compose version. See JetBrains' [compatibility table](https://kotlinlang.org/docs/multiplatform/compose-compatibility-and-versioning.html).

## Running

`kargo run` starts the application with `-Dcompose.application.configure.swing.globals=true`, as the Compose Gradle plugin does. On macOS it also passes the application's name and icon to the Dock (`-Xdock:name`, `-Xdock:icon`) from `[package.installer]`, so a development build looks like the installed one:

```toml title="kargo.toml"
[package.installer]
name = "Notes"
icon = { windows = "icons/notes.ico", macos = "icons/notes.icns", linux = "icons/notes.png" }
```

On Windows and Linux the taskbar shows the window's title and the icon you pass to `Window(icon = ...)`.

## Hot reload

```console
$ kargo run --hot-reload
     Running `java com.example.counter.MainKt`
     Changed src/App.kt
   Compiling counter v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.64s
    Reloaded 2 classes in 210 ms
```

`--hot-reload` (or `--hot`) works like [`--watch`](../../reference/commands/#kargo-run-r): Kargo rebuilds when a file changes. The desktop app keeps running, though. Kargo swaps the classes that changed into it and Compose recomposes the window with the new code, keeping what `remember` holds, such as the click count. A build that fails leaves the app as it was.

The app runs on [JetBrains Runtime](https://github.com/JetBrains/JetBrainsRuntime) (JBR) with `-XX:+AllowEnhancedClassRedefinition`, so new functions, fields and classes reload too. Kargo downloads the JBR of your project's Java version into `~/.kargo/toolchains/jbr/` the first time (about 300 MB) and checks it against the SHA-512 checksum JetBrains publishes. Offline, or where there is no JBR, the app runs on the project's JDK with a warning, and only changes inside function bodies reload.

Kargo restarts the app instead when a change cannot be applied in place, and when the command line changes, as with a new dependency or JVM option. The app runs from a copy of its classes in `target/<profile>/jvm/hot-reload/`, which also holds `agent.log`, a record of each reload.

`--hot-reload` needs the jvm target. It takes the options of `--watch` (`--clear`, `--ignore`), and not `--debug` or `--native-agent`. Without Compose it still swaps classes into any other JVM program, on your project's own JDK; see [Hot reload a JVM program](../watch/#hot-reload-a-jvm-program).

## Previews

Annotate a top-level `@Composable` function without parameters with `@Preview`, and `kargo preview` renders it to a PNG file:

```kotlin title="src-jvm/Main.kt"
import androidx.compose.runtime.Composable
import androidx.compose.ui.tooling.preview.Preview

@Preview
@Composable
fun AppPreview() {
    App()
}
```

```console
$ kargo preview --list
com.example.counter.AppPreview	src-jvm/Main.kt:18
$ kargo preview --density 2
   Compiling the preview renderer
   Rendering 1 preview
    Rendered target/preview/com.example.counter.AppPreview.png
```

When the project has no previews, `kargo preview --list` prints the note `no @Preview functions found` on stderr and nothing on stdout.

Without names it renders every preview; `kargo preview AppPreview` renders one, by its function or qualified name. Each image is `--width` by `--height` density-independent pixels (800 by 600 by default) times `--density`. It is drawn off screen, without a window, so `kargo preview` also works on a CI machine without a display. `--out-dir` writes the files somewhere other than `target/preview/`.

Previews come from the JVM sources (`src/` and `src-jvm/`) and are drawn by Compose Desktop, so the project needs the jvm target. Any of the Compose `@Preview` annotations counts. A preview with parameters (`@PreviewParameter`) or inside a class is left out.

Editors see them too: [`kargo lsp`](../editors/) lists each preview as a runnable of kind `preview`, and the IntelliJ plugin puts a render icon in the gutter of each one and a Previews group in the Kargo tool window.

## Compose for the web

Compose for Web draws the UI on a canvas with Skia compiled to WebAssembly. Enable Compose and the Wasm target for the browser:

```toml title="kargo.toml"
[target.wasm]
environment = "browser"

[frameworks]
compose = true
```

```kotlin title="src-wasm/Main.kt"
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.window.ComposeViewport
import kotlinx.browser.document

@OptIn(ExperimentalComposeUiApi::class)
fun main() {
    ComposeViewport(document.body!!) {
        App()
    }
}
```

`kargo run -t wasm` builds the app and serves it. `kargo build -t wasm` writes the site into `target/<profile>/wasm/`:

- the module (`<name>.mjs`, `<name>.wasm`) and Skiko's runtime (`skiko.mjs`, `skiko.wasm`), taken from the `skiko-js-wasm-runtime` jar the Compose libraries depend on,
- `composeResources/`, the files `Res` fetches,
- an `index.html` whose canvas fills the window, unless `resources/index.html` is your own page.

Compose for Web needs `environment = "browser"`; with `js` or `wasi` the build stops with an error. A release build runs `wasm-opt` on the module and reports its size, as for any [Wasm app](../wasm/).

## Compose HTML (DOM elements)

[Compose HTML](https://github.com/JetBrains/compose-multiplatform/tree/master/html) (`org.jetbrains.compose.html`) renders real DOM elements from composables (`Div`, `Text`, `Button`, a style DSL), so the page can be styled with CSS, selected and read by assistive technology. It is not the same as [Compose for the web](#compose-for-the-web), which draws on a canvas. Compose HTML runs on **Kotlin/JS only**; Kotlin/Wasm doesn't support it. Create a project with the `compose-html` preset:

```console
$ kargo new counter --with compose-html
$ cd counter
$ kargo run
     Serving target/debug/js at http://127.0.0.1:8080/ (press Ctrl+C to stop)
```

The preset writes the JS target for a browser and turns Compose HTML on:

```toml title="kargo.toml"
[target.js]
environment = "browser"

[frameworks]
compose = { html = true }
```

```kotlin title="src/Main.kt"
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import org.jetbrains.compose.web.dom.Button
import org.jetbrains.compose.web.dom.Div
import org.jetbrains.compose.web.dom.Text
import org.jetbrains.compose.web.renderComposable

fun main() {
    renderComposable(rootElementId = "root") {
        var clicks by remember { mutableStateOf(0) }
        Div { Text("Hello, Compose HTML!") }
        Button(attrs = { onClick { clicks++ } }) { Text("Clicked $clicks times") }
    }
}
```

With `html = true`, the JS target gets the Compose compiler plugin, `org.jetbrains.compose.runtime:runtime` and `org.jetbrains.compose.html:html-core`, at the Compose version of `[frameworks.compose]`. `svg = true` adds `html-svg`. Material 3 and the canvas libraries aren't added, as they don't run on Kotlin/JS. The Compose compiler options (`stability-config`, `strong-skipping`, `feature-flags`, `metrics`, `reports`) apply as on every other target.

The generated page has `<div id="root"></div>` before the script, so `renderComposable(rootElementId = "root")` finds its element; if you write your own `resources/index.html`, keep an element with the id you pass. `kargo run`, `--watch` and `kargo test` (in a headless browser) work as for any [browser app on the JS target](../js/#browser-apps). `kargo build --release` links with dead code elimination and prints a `Size` line with the size of the module (raw, gzip and brotli) and of the whole output, like a [Wasm build](../wasm/).

Compose HTML needs `[target.js]` with `environment = "browser"`:

- Compose on the JS target without `html = true` is an error that points here and to [Compose for the web](#compose-for-the-web).
- `html = true` with only `[target.wasm]` is an error: add `[target.js]` (`kargo target add js`), or remove `html` to draw on a canvas.

### With the other targets

`html = true` changes the JS target only. In a project with several targets, the others keep their own Compose: the JVM and Wasm targets get Compose Multiplatform for desktop and canvas, Android gets Jetpack Compose. Put the Compose HTML entry point in `src-js/` and the canvas one in `src-wasm/`, share what doesn't draw (state, models, logic) in `src/`, and use `expect`/`actual` for the screens that differ:

```toml title="kargo.toml"
[target.js]
environment = "browser"

[target.wasm]
environment = "browser"

[frameworks]
compose = { html = true }
```

```kotlin title="src/Screen.kt"
import androidx.compose.runtime.Composable

@Composable
expect fun Screen(state: CounterState)
```

Libraries that exist only for Compose HTML (`org.jetbrains.compose.html:*`) belong in `[target.js.dependencies]`, so `kargo check` on the other targets doesn't resolve them. See [Multiplatform](../multiplatform/).

## iOS

Compose for iOS is a framework that an Xcode project shows through a `UIViewController`. Set the Native target to iOS platforms and `kind = "framework"`:

```toml title="kargo.toml"
[target.native]
platforms = ["ios_arm64", "ios_simulator_arm64"]
kind = "framework"

[frameworks]
compose = true
```

```kotlin title="src-native/MainViewController.kt"
import androidx.compose.ui.window.ComposeUIViewController
import platform.UIKit.UIViewController

fun MainViewController(): UIViewController = ComposeUIViewController { App() }
```

`kargo build -t native` writes `target/<profile>/native/<platform>/<name>.framework`, and `kargo build -t native --xcframework` one `<name>.xcframework` for the devices and the simulators (see [Kotlin/Native](../native/)). In Swift, show the controller:

```swift title="ContentView.swift"
import SwiftUI
import counter

struct ComposeView: UIViewControllerRepresentable {
    func makeUIViewController(context: Context) -> UIViewController { MainViewControllerKt.MainViewController() }
    func updateUIViewController(_ uiViewController: UIViewController, context: Context) {}
}
```

Compose for iOS reads resources from a `compose-resources` folder in the app bundle. Kargo writes that folder to `target/<profile>/native/compose-resources/`; add it to the app target's Copy Bundle Resources phase as a folder reference.

The platforms can be `ios_arm64`, `ios_simulator_arm64` and `ios_x64`. Building them needs macOS with Xcode. On any other host, `kargo new -t native` writes the `[target.native]` table commented out, so `kargo build` does not start a build that fails; uncomment it on a Mac.

## Distributing

`kargo package --desktop` builds an installer for the OS it runs on: an `.msi` on Windows, a `.dmg` on macOS and a `.deb` on Linux, with a Java runtime trimmed by `jlink`, the application's name and icon and a Start menu or launcher entry:

```toml title="kargo.toml"
[package.installer]
name = "Notes"
vendor = "Example"
icon = { windows = "icons/notes.ico", macos = "icons/notes.icns", linux = "icons/notes.png" }
type = ["msi", "dmg", "deb"]
menu = true
file-associations = [{ extension = "note", mime-type = "application/x-note", description = "Note" }]
```

```sh
kargo package --desktop --release
```

`type` lists one installer per OS; each machine builds the ones for its OS, so the same `kargo.toml` works on a Windows, a macOS and a Linux CI runner. [Distributing applications](../distributing/#installers) describes every `[package.installer]` key.

For Compose, the installer adds what the Compose Gradle plugin's `nativeDistributions` would:

- the JDK modules Compose needs (`java.desktop`, `java.logging`, `jdk.unsupported`) besides the ones `jdeps` finds;
- Skiko's native library unpacked next to the jars (`-Dskiko.library.path=$APPDIR`) instead of inside a jar, so macOS signs and notarizes it with the rest of the application;
- `-Dcompose.application.configure.swing.globals=true` and, on JDK 21 and newer, `--enable-native-access=ALL-UNNAMED`.

Add `[target.jvm] jvm-args` or a profile's `jvm-args` for options of your own, such as `-Xmx512m`.

### Signing and notarizing on macOS

Gatekeeper opens a downloaded application only when it is signed with a Developer ID and notarized by Apple. Set the signing identity and the notarization credentials:

```toml title="kargo.toml"
[package.installer]
bundle-id = "com.example.notes"
signing-identity = "Developer ID Application: Example Inc (ABCDE12345)"
notarize = { apple-id-env = "APPLE_ID", password-env = "APPLE_APP_PASSWORD", team-id = "ABCDE12345" }
```

```sh
export APPLE_ID=you@example.com
export APPLE_APP_PASSWORD=abcd-efgh-ijkl-mnop   # an app-specific password
kargo package --desktop --release
```

`jpackage` signs the application, Skiko's library included. Kargo then sends each `.dmg` and `.pkg` to Apple's notary service with `xcrun notarytool submit --wait` and staples the ticket to it with `xcrun stapler staple`. The credentials stay in environment variables, so `kargo.toml` holds no secret. With credentials that `xcrun notarytool store-credentials` saved in the keychain, use `notarize = { keychain-profile = "notes" }` instead. On Windows and Linux, `notarize` is ignored.

### One jar for every OS

```sh
kargo package --desktop --format jar --platforms all
java -jar target/debug/jvm/notes-0.1.0-all.jar
```

A plain `kargo package` jar carries Skiko's library for the OS it was built on only. `--platforms` sets the platforms whose libraries it carries: `all`, or a list of `windows-x64`, `windows-arm64`, `macos-x64`, `macos-arm64`, `linux-x64` and `linux-arm64`. Skiko loads the right one at startup. The jar needs a Java runtime on the machine that runs it, and each platform adds about 10 MB to it.

Release desktop jars are shrunk and obfuscated with R8. Add reflection keep rules to [`proguard-rules.pro`](../../reference/manifest/#packageoptimize) when the app or its dependencies load classes by name.

## Android

The same `compose = true` turns on Jetpack Compose for the [Android target](../android/). `kargo new app -t android -w compose` creates a Compose `MainActivity`. `composeResources/` and `Res` work there too.
