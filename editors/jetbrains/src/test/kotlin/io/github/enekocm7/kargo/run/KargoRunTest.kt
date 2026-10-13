package io.github.enekocm7.kargo.run

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class KargoRunTest {
    @Test
    fun `test runs get JSON messages`() {
        assertEquals(listOf("test", "--message-format", "json", "MathTest"), KargoCommandState.kargoArgs(listOf("test", "MathTest"), true, null))
        assertEquals(listOf("test", "--message-format=human"), KargoCommandState.kargoArgs(listOf("test", "--message-format=human"), true, null))
    }

    @Test
    fun `debugging picks the port and the JVM target`() {
        assertEquals(listOf("run", "--debug=5006", "-t", "jvm"), KargoCommandState.kargoArgs(listOf("run"), false, 5006))
        assertEquals(listOf("run", "--debug=5006", "--bin", "x", "-t", "jvm"), KargoCommandState.kargoArgs(listOf("run", "--bin", "x", "-t", "jvm"), false, 5006))
        // `-t` after `--` belongs to the program.
        assertEquals(listOf("run", "--debug=1", "-t", "jvm", "--", "-t"), KargoCommandState.kargoArgs(listOf("run", "--", "-t"), false, 1))
    }

    @Test
    fun `console links compiler locations`() {
        val match = KargoConsoleFilter.LOCATION.find("src/Main.kt:2:16: error: initializer type mismatch")!!
        assertEquals(listOf("src/Main.kt", "2", "16"), match.groupValues.drop(1))
        val windows = KargoConsoleFilter.LOCATION.find("""C:\app\src\Main.kt:3:1: warning: unused""")!!
        assertEquals("""C:\app\src\Main.kt""", windows.groupValues[1])
        assertNull(KargoConsoleFilter.LOCATION.find("Compiling app v0.1.0 (jvm)"))
    }

    @Test
    fun `test results become service messages`() {
        val translator = KargoTestTranslator()
        val id = "[engine:junit-jupiter]/[class:com.example.MathTest]/[method:adds()]"
        val parent = "[engine:junit-jupiter]/[class:com.example.MathTest]"
        val common = """"package":"app","target":"jvm","id":"$id","parent-id":"$parent","suite":"com.example.MathTest","name":"adds","display-name":"adds()""""

        val started = translator.translate("""{"reason":"test-started",$common}""")!!
        assertEquals(2, started.size)
        assertTrue(started[0], started[0].startsWith("##teamcity[testSuiteStarted") && started[0].contains("name='MathTest'"))
        assertTrue(started[1], started[1].startsWith("##teamcity[testStarted") && started[1].contains("name='adds()'"))

        val finished = translator.translate("""{"reason":"test-finished",$common,"outcome":"failed","message":"expected 2","duration-ms":12}""")!!
        assertEquals(2, finished.size)
        assertTrue(finished[0], finished[0].startsWith("##teamcity[testFailed") && finished[0].contains("message='expected 2'"))
        assertTrue(finished[1], finished[1].startsWith("##teamcity[testFinished") && finished[1].contains("duration='12'"))

        val summary = translator.translate("""{"reason":"test-summary","package":"app","target":"jvm","passed":0,"failed":1,"ignored":0,"success":false}""")!!
        assertEquals(1, summary.size)
        assertTrue(summary[0], summary[0].startsWith("##teamcity[testSuiteFinished"))

        assertNull(translator.translate("test MathTest.adds ... FAILED"))
        assertEquals(emptyList<String>(), translator.translate("""{"reason":"build-finished","success":true}"""))
    }

    @Test
    fun `ignored tests are started first`() {
        val messages = KargoTestTranslator().translate(
            """{"reason":"test-finished","package":"app","target":"native","id":"a.B.c","parent-id":null,"suite":"a.B","name":"c","outcome":"ignored"}""",
        )!!
        assertEquals(listOf("testSuiteStarted", "testStarted", "testIgnored", "testFinished"), messages.map { it.removePrefix("##teamcity[").substringBefore(' ') })
        assertTrue(messages[0], messages[0].contains("name='B (native)'"))
    }
}
