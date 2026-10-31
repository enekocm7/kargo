package com.example.composemultiplatform

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier

fun greeting(name: String) = "Hello, $name!"

/** The app's UI, shared by every target. */
@Composable
fun App() {
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
