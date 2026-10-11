---
title: Compose Desktop
description: Build a desktop GUI with Compose Multiplatform.
---

[Compose Multiplatform](https://www.jetbrains.com/compose-multiplatform/) lets you build desktop user interfaces in Kotlin. Kargo sets up the compiler plugin, the right native runtime for your OS and Material 3.

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

## Distributing

```sh
kargo package --release
java -jar target/release/jvm/counter-0.1.0-all.jar
```

The jar contains the Compose runtime for the OS you built on. Build on each OS you want to ship for.

## Android

The same `compose = true` turns on Jetpack Compose for the [Android target](../android/). `kargo new app -t android -w compose` creates a Compose `MainActivity`.
