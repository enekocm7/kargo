package io.github.enekocm7.kargo.projecttask

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class KargoDiagnosticsTest {
    private val error = """{"reason":"compiler-message","severity":"error","message":"Unresolved reference 'foo'","file":"/app/src/Main.kt","line":3,"column":9,"rendered":"e: /app/src/Main.kt:3:9 Unresolved reference 'foo'","target":"jvm"}"""

    @Test
    fun `a compiler message becomes a diagnostic with its location`() {
        val diagnostic = KargoDiagnosticReader.parse(error)!!
        assertEquals("error", diagnostic.severity)
        assertEquals("Unresolved reference 'foo'", diagnostic.message)
        assertEquals("/app/src/Main.kt", diagnostic.file)
        assertEquals(3, diagnostic.line)
        assertEquals(9, diagnostic.column)
        assertEquals("e: /app/src/Main.kt:3:9 Unresolved reference 'foo'", diagnostic.rendered)
    }

    @Test
    fun `a message without a location keeps the location empty`() {
        val diagnostic = KargoDiagnosticReader.parse("""{"reason":"compiler-message","severity":"warning","message":"-Werror","file":null,"line":null,"column":null,"rendered":"warning: -Werror"}""")!!
        assertEquals("warning", diagnostic.severity)
        assertNull(diagnostic.file)
        assertNull(diagnostic.line)
        assertNull(diagnostic.column)
    }

    @Test
    fun `other lines are not diagnostics`() {
        assertNull(KargoDiagnosticReader.parse("Compiling app v0.1.0 (jvm)"))
        assertNull(KargoDiagnosticReader.parse("""{"reason":"build-finished","success":false}"""))
        assertNull(KargoDiagnosticReader.parse("{not json"))
    }

    @Test
    fun `a line split across chunks is read once it is complete`() {
        val reader = KargoDiagnosticReader()
        val (first, second) = error.split(",\"line\"", limit = 2).let { it[0] to ",\"line\"" + it[1] }
        assertEquals(emptyList<KargoDiagnostic>(), reader.accept(first))
        assertEquals(emptyList<KargoDiagnostic>(), reader.accept(second.substring(0, 5)))
        val messages = reader.accept(second.substring(5) + "\nCompiling app\n" + error + "\n")
        assertEquals(listOf(3, 3), messages.map { it.line })
    }

    @Test
    fun `the last line is read even without a newline`() {
        val reader = KargoDiagnosticReader()
        assertEquals(emptyList<KargoDiagnostic>(), reader.accept(error))
        assertEquals("Unresolved reference 'foo'", reader.finish().single().message)
        assertEquals(emptyList<KargoDiagnostic>(), reader.finish())
    }
}
