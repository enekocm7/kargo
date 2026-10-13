package io.github.enekocm7.kargo.server.documents

import org.eclipse.lsp4j.Position
import org.eclipse.lsp4j.Range
import org.eclipse.lsp4j.TextDocumentContentChangeEvent
import java.nio.file.Paths
import kotlin.test.Test
import kotlin.test.assertEquals

class DocumentsTest {
    @Test
    fun convertsPositionsInUtf16Units() {
        // `😀` is two UTF-16 units; `\r\n` ends the first line.
        val index = LineIndex("val a = \"😀\"\r\nfun b() {}\n")
        assertEquals(11, index.offset(Position(0, 11)))
        assertEquals(Position(0, 11), index.position(11))
        assertEquals(14, index.offset(Position(1, 0)))
        assertEquals(13, index.offset(Position(0, 99)), "clamped to the end of the line, before the newline")
        assertEquals(Position(1, 4), index.position(18))
        assertEquals(25, index.offset(Position(9, 0)), "past the last line: the end of the text")
    }

    @Test
    fun appliesIncrementalChanges() {
        val documents = Documents()
        documents.open("file:///p/src/Main.kt", 1, "fun main() {\n}\n")
        val insert = TextDocumentContentChangeEvent(Range(Position(0, 12), Position(0, 12)), "\n    println()")
        val replace = TextDocumentContentChangeEvent(Range(Position(1, 4), Position(1, 11)), "print")
        val changed = documents.change("file:///p/src/Main.kt", 3, listOf(insert, replace))!!
        assertEquals("fun main() {\n    print()\n}\n", changed.text)
        assertEquals(3, changed.version)
        val whole = documents.change("file:///p/src/Main.kt", 4, listOf(TextDocumentContentChangeEvent("x")))!!
        assertEquals("x", whole.text)
    }

    @Test
    fun normalizesFileUris() {
        val path = uriToPath("file:///p/src/A%20B.kt")!!
        assertEquals(normalize(Paths.get("/p/src/A B.kt")), path)
        assertEquals(null, uriToPath("jar:file:///x.jar!/A.kt"))
        if (System.getProperty("os.name").startsWith("Windows")) {
            // Editors send lower-case and escaped drive letters; the model has upper-case ones.
            assertEquals(Paths.get("C:\\p\\Main.kt"), uriToPath("file:///c%3A/p/Main.kt"))
            assertEquals("file:///C:/p/Main.kt", pathToUri(Paths.get("c:\\p\\Main.kt")))
        }
    }
}
