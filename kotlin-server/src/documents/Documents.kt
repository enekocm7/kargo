// Documents open in the editor: their text as the editor has it, which may differ from the file
// on disk, and conversions between LSP positions and offsets.
//
// LSP positions count UTF-16 code units, as Kotlin strings do, so a column is an index into the
// line. Lines end at `\n`; a `\r` before it belongs to the line.
package io.github.enekocm7.kargo.server.documents

import org.eclipse.lsp4j.Position
import org.eclipse.lsp4j.Range
import org.eclipse.lsp4j.TextDocumentContentChangeEvent
import java.net.URI
import java.nio.file.Path
import java.nio.file.Paths
import java.util.concurrent.ConcurrentHashMap

class Document(val uri: String, val path: Path?, val version: Int, val text: String) {
    val lines: LineIndex by lazy { LineIndex(text) }

    /** The document after the changes of a `didChange`, in order. */
    fun changed(version: Int, changes: List<TextDocumentContentChangeEvent>): Document {
        var text = text
        for (change in changes) {
            val range = change.range
            text = if (range == null) {
                change.text
            } else {
                val index = LineIndex(text)
                val start = index.offset(range.start)
                val end = maxOf(start, index.offset(range.end))
                text.substring(0, start) + change.text + text.substring(end)
            }
        }
        return Document(uri, path, version, text)
    }
}

/** The start of every line of a text. */
class LineIndex(private val text: String) {
    private val starts: IntArray = buildList {
        add(0)
        text.forEachIndexed { i, c -> if (c == '\n') add(i + 1) }
    }.toIntArray()

    /** The offset of an LSP position, clamped to the text and to the end of its line. */
    fun offset(position: Position): Int {
        if (position.line < 0) return 0
        if (position.line >= starts.size) return text.length
        val start = starts[position.line]
        val end = if (position.line + 1 < starts.size) starts[position.line + 1] - 1 else text.length
        return (start + position.character.coerceAtLeast(0)).coerceAtMost(end)
    }

    /** The LSP position of an offset. */
    fun position(offset: Int): Position {
        val clamped = offset.coerceIn(0, text.length)
        var line = starts.binarySearch(clamped)
        if (line < 0) line = -line - 2
        return Position(line, clamped - starts[line])
    }

    fun range(start: Int, end: Int): Range = Range(position(start), position(end))
}

/** The documents open in the editor, by URI. */
class Documents {
    private val open = ConcurrentHashMap<String, Document>()

    fun open(uri: String, version: Int, text: String): Document = Document(uri, uriToPath(uri), version, text).also { open[uri] = it }

    fun change(uri: String, version: Int, changes: List<TextDocumentContentChangeEvent>): Document? =
        open.computeIfPresent(uri) { _, doc -> doc.changed(version, changes) }

    fun close(uri: String) {
        open.remove(uri)
    }

    operator fun get(uri: String): Document? = open[uri]

    fun all(): List<Document> = open.values.toList()
}

/**
 * The path of a `file:` URI, absolute and normalized, with an upper-case drive letter on
 * Windows, as Kargo writes paths in the project model.
 */
fun uriToPath(uri: String): Path? {
    val parsed = runCatching { URI(uri) }.getOrNull() ?: return null
    if (parsed.scheme != "file") return null
    val path = runCatching { Paths.get(parsed) }.getOrNull() ?: return null
    return normalize(path)
}

/** An absolute, normalized path with an upper-case Windows drive letter. */
fun normalize(path: Path): Path {
    val text = path.toAbsolutePath().normalize().toString()
    return if (text.length >= 2 && text[1] == ':' && text[0].isLetter()) {
        Paths.get(text[0].uppercaseChar() + text.substring(1))
    } else {
        Paths.get(text)
    }
}

/** The `file:` URI of a path, with an upper-case drive letter. */
fun pathToUri(path: Path): String = normalize(path).toUri().toString()
