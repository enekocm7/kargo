package com.example.multiplatformlibrary

import kotlin.test.Test
import kotlin.test.assertEquals

class GreetingTest {
    @Test
    fun greetsByName() {
        assertEquals("Hello, Kargo!", greeting("Kargo"))
    }
}
