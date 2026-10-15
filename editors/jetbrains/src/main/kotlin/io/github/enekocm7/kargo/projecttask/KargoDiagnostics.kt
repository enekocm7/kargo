// The diagnostics of `kargo build --message-format json`: one `compiler-message` JSON object per line of
// stdout, which the Build tool window shows as build messages. Kept free of IDE types, so it can be
// unit tested without the IntelliJ platform.

package io.github.enekocm7.kargo.projecttask

import com.google.gson.JsonObject
import com.google.gson.JsonParser

/** One compiler message. `file`, `line` and `column` are null when the compiler gave no location. */
data class KargoDiagnostic(
    /** `error`, `warning`, `info`, `exception` or `note`. */
    val severity: String,
    val message: String,
    val file: String?,
    /** 1-based, as the compiler prints it. */
    val line: Int?,
    /** 1-based, as the compiler prints it. */
    val column: Int?,
    /** The diagnostic as the compiler printed it, with the source excerpt. */
    val rendered: String,
)

/** Turns the lines of `kargo build --message-format json` into diagnostics as the output arrives. */
class KargoDiagnosticReader {
    private val pending = StringBuilder()

    /** The diagnostics on the complete lines of `text`; a line split across chunks waits for its end. */
    fun accept(text: String): List<KargoDiagnostic> {
        pending.append(text)
        val end = pending.lastIndexOf("\n")
        if (end < 0) return emptyList()
        val complete = pending.substring(0, end)
        pending.delete(0, end + 1)
        return complete.split('\n').mapNotNull(::parse)
    }

    /** Whatever is left when the output ends without a newline. */
    fun finish(): List<KargoDiagnostic> {
        val rest = pending.toString()
        pending.clear()
        return parse(rest)?.let { listOf(it) }.orEmpty()
    }

    companion object {
        /** The diagnostic in one line, or `null` when the line is not a `compiler-message`. */
        fun parse(line: String): KargoDiagnostic? {
            val trimmed = line.trim()
            if (!trimmed.startsWith("{")) return null
            val json = runCatching { JsonParser.parseString(trimmed).asJsonObject }.getOrNull() ?: return null
            if (json.string("reason") != "compiler-message") return null
            return KargoDiagnostic(
                severity = json.string("severity") ?: "error",
                message = json.string("message") ?: "",
                file = json.string("file"),
                line = json.int("line"),
                column = json.int("column"),
                rendered = json.string("rendered") ?: json.string("message") ?: "",
            )
        }

        private fun JsonObject.string(key: String): String? = get(key)?.takeIf { it.isJsonPrimitive }?.asString

        private fun JsonObject.int(key: String): Int? = get(key)?.takeIf { it.isJsonPrimitive }?.asInt
    }
}
