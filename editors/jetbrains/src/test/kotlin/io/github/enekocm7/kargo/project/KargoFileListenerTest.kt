package io.github.enekocm7.kargo.project

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class KargoFileListenerTest {
    @Test
    fun `directories that hold roots are watched`() {
        for (name in listOf("src", "src-jvm", "src-wasm", "src-native", "tests", "tests-jvm", "resources", "test-resources")) {
            assertTrue(name, KargoFileListener.LAYOUT_DIR.matches(name))
        }
    }

    @Test
    fun `other names are not`() {
        for (name in listOf("target", "examples", "docs", "src-", "source", "tests.kt", "src-JVM", "build")) {
            assertFalse(name, KargoFileListener.LAYOUT_DIR.matches(name))
        }
    }

    @Test
    fun `paths are compared with forward slashes`() {
        assertEquals("C:/p/app", norm("C:\\p\\app\\"))
        assertEquals("/", norm("/"))
    }
}
