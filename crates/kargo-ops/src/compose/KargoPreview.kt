// The renderer behind `kargo preview`. Kargo compiles it against the project's classes with the
// Compose compiler and runs it with: <width> <height> <density> <class>|<function>|<png file>...
//
// Each preview is a top-level `@Composable` function without parameters, so its JVM method takes
// only the composer and the `$changed` bits; it is called through reflection, as IDE previews do,
// inside an off-screen `ImageComposeScene`, and the first frame is written as a PNG.
@file:OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)

package kargo.preview

import androidx.compose.runtime.Composable
import androidx.compose.runtime.currentComposer
import androidx.compose.ui.ImageComposeScene
import androidx.compose.ui.unit.Density
import org.jetbrains.skia.EncodedImageFormat
import java.io.File
import java.lang.reflect.InvocationTargetException
import java.lang.reflect.Method
import kotlin.system.exitProcess

fun main(args: Array<String>) {
    val width = args[0].toInt()
    val height = args[1].toInt()
    val density = args[2].toFloat()
    var failed = false
    for (spec in args.drop(3)) {
        val (className, functionName, out) = spec.split('|', limit = 3)
        try {
            val method = Class.forName(className).declaredMethods.first { it.name == functionName && it.parameterCount == 2 }
            method.isAccessible = true
            val scene = ImageComposeScene(width, height, Density(density)) { Preview(method) }
            try {
                val image = scene.render()
                val data = image.encodeToData(EncodedImageFormat.PNG) ?: error("the frame could not be encoded as PNG")
                File(out).apply { parentFile?.mkdirs() }.writeBytes(data.bytes)
                println("rendered $out")
            } finally {
                scene.close()
            }
        } catch (e: Throwable) {
            val cause = if (e is InvocationTargetException) e.cause ?: e else e
            System.err.println("failed $className.$functionName: $cause")
            failed = true
        }
    }
    // Compose and coroutines may leave threads behind; the work is done.
    exitProcess(if (failed) 1 else 0)
}

@Composable
private fun Preview(method: Method) {
    method.invoke(null, currentComposer, 0)
}
