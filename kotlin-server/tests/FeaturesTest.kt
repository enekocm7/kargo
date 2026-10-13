package io.github.enekocm7.kargo.server.features

import io.github.enekocm7.kargo.server.server.minorVersion
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class FeaturesTest {
    @Test
    fun rendersKdocAsMarkdown() {
        val kdoc = "/**\n     * Greets [name].\n     *\n     * @param name who\n     */"
        assertEquals("Greets [name].\n\n@param name who", kdocText(kdoc))
    }

    @Test
    fun comparesKotlinVersions() {
        assertEquals(minorVersion("2.4.21"), minorVersion("2.4.20-dev-6138"))
        assertTrue(minorVersion("2.5.0") > minorVersion("2.4.20-dev-6138"))
    }
}
