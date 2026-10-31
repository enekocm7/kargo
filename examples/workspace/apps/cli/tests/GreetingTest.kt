package com.example.cli

import com.example.greeter.greeting
import kotlin.test.Test
import kotlin.test.assertEquals

class GreetingTest {
    @Test
    fun usesTheGreeterLibrary() {
        assertEquals("Hello, Kargo!", greeting("Kargo"))
    }
}
