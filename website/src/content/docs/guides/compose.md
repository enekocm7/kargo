---
title: Compose Desktop
description: Build a desktop GUI with Compose Multiplatform, use Compose resources and ship installers for Windows, macOS and Linux.
---

[Compose Multiplatform](https://www.jetbrains.com/compose-multiplatform/) lets you build desktop user interfaces in Kotlin. Kargo sets up the compiler plugin, the right native runtime for your OS, Material 3 and Compose resources, and packages the application as an installer.

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

`Res` lives in `<group>.<name>.generated.resources` (lower case, `-` as `_`) and is `internal`. Both are configurable:

```toml title="kargo.toml"
[frameworks]
compose = { resources-package = "com.example.notes.res", public-resources = true }
```

Kargo adds the resources runtime (`org.jetbrains.compose.components:components-resources`) once `composeResources/` exists, writes `Res.kt` into `target/<profile>/jvm/generated/compose-resources/` on every build and rebuilds when a resource changes, also under `kargo run --watch`. The same directory works for the [Android target](../android/), where the files become assets of the APK.

## Running

`kargo run` starts the application with `-Dcompose.application.configure.swing.globals=true`, as the Compose Gradle plugin does. On macOS it also passes the application's name and icon to the Dock (`-Xdock:name`, `-Xdock:icon`) from `[package.installer]`, so a development build looks like the installed one:

```toml title="kargo.toml"
[package.installer]
name = "Notes"
icon = { windows = "icons/notes.ico", macos = "icons/notes.icns", linux = "icons/notes.png" }
```

On Windows and Linux the taskbar shows the window's title and the icon you pass to `Window(icon = ...)`.

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

## Android

The same `compose = true` turns on Jetpack Compose for the [Android target](../android/). `kargo new app -t android -w compose` creates a Compose `MainActivity`. `composeResources/` and `Res` work there too.
