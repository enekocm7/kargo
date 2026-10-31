package com.example.composemultiplatform

import androidx.compose.runtime.Composable
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application

fun main() =
    application {
        Window(onCloseRequest = ::exitApplication, title = "compose-multiplatform") {
            App()
        }
    }

// `kargo preview` renders this into target/preview/.
@Preview
@Composable
fun AppPreview() {
    App()
}
