package com.example.composehtml

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import org.jetbrains.compose.web.dom.Button
import org.jetbrains.compose.web.dom.Div
import org.jetbrains.compose.web.dom.Text
import org.jetbrains.compose.web.renderComposable

fun greeting(name: String) = "Hello, $name!"

// `kargo run` serves the page at http://127.0.0.1:8080/; Kargo's page has the `root` element.
fun main() {
    renderComposable(rootElementId = "root") {
        var clicks by remember { mutableStateOf(0) }
        Div { Text(greeting("Compose HTML")) }
        Button(attrs = { onClick { clicks++ } }) { Text("Clicked $clicks times") }
    }
}
