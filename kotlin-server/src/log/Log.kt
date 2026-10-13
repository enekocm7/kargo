// The server's log: `target/lsp/kotlin-server.log` once the project model names the target
// directory, and stderr, which editors keep in the language server's log.
package io.github.enekocm7.kargo.server.log

import java.io.PrintWriter
import java.io.StringWriter
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter

object Log {
    /** The log is started again above this size, keeping the previous one as `.1`. */
    private const val MAX_SIZE = 5L * 1024 * 1024

    @Volatile
    private var file: Path? = null

    /** Whether debug messages are logged: when the editor asks for a `verbose` trace. */
    @Volatile
    var verbose: Boolean = false

    fun to(file: Path?) {
        if (file == null) return
        runCatching {
            Files.createDirectories(file.parent)
            if (Files.exists(file) && Files.size(file) > MAX_SIZE) {
                Files.move(file, file.resolveSibling("${file.fileName}.1"), java.nio.file.StandardCopyOption.REPLACE_EXISTING)
            }
        }
        this.file = file
    }

    fun info(message: String) = write("INFO", message)

    fun debug(message: String) {
        if (verbose) write("DEBUG", message)
    }

    fun error(message: String, error: Throwable? = null) {
        val trace = error?.let { e -> StringWriter().also { e.printStackTrace(PrintWriter(it)) }.toString() }
        write("ERROR", if (trace == null) message else "$message\n$trace")
    }

    @Synchronized
    private fun write(level: String, message: String) {
        val line = "${LocalDateTime.now().format(DateTimeFormatter.ISO_LOCAL_DATE_TIME)} $level $message\n"
        System.err.print(line)
        file?.let { runCatching { Files.writeString(it, line, StandardCharsets.UTF_8, StandardOpenOption.CREATE, StandardOpenOption.APPEND) } }
    }
}
