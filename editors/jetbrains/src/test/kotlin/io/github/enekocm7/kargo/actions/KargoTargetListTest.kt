package io.github.enekocm7.kargo.actions

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class KargoTargetListTest {
    @Test
    fun `remove target offers the enabled targets`() {
        val json = """{"enabled":[{"name":"jvm","implicit":false,"settings":{}},{"name":"wasm","implicit":false,"settings":{"browser":"true"}}],"available":["native","android"]}"""
        assertEquals(listOf("jvm", "wasm"), enabledTargetNames(json))
    }

    @Test
    fun `a package without enabled targets offers none`() {
        assertTrue(enabledTargetNames("""{"enabled":[],"available":["jvm"]}""").isEmpty())
    }
}
