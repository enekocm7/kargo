package io.github.enekocm7.kargo.server.model

import com.google.gson.JsonParser
import java.nio.file.Paths
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ModelTest {
    private val json = """
        {
          "modelVersion": 1,
          "root": "file:///p",
          "targetDir": "/p/target",
          "resolved": true,
          "jdk": { "home": "/jdk", "version": 21 },
          "kotlin": { "projectVersion": "2.4.21" },
          "libraries": [
            { "id": "g:a:1", "kind": "jar", "path": "/c/a-1.jar", "sources": null, "sha256": "aa", "platform": "jvm" }
          ],
          "modules": [
            {
              "id": "app.jvm.main", "package": "app", "member": true, "sourceSet": "jvm", "scope": "main",
              "platform": ["jvm"], "sourceRoots": ["/p/src", "/p/src-jvm"], "resourceRoots": [], "generatedRoots": [],
              "dependsOn": [], "friends": [], "dependencies": ["module:lib.jvm.main", "lib:g:a:1"],
              "compiler": {
                "languageVersion": "2.2", "apiVersion": null, "optIn": ["kotlin.RequiresOptIn"], "warningsAsErrors": false,
                "multiplatform": false, "jvmTarget": "21", "freeArgs": ["-Xcontext-parameters"],
                "plugins": [{ "id": "org.jetbrains.kotlinx.serialization", "module": "g:p", "options": [{ "name": "k", "value": "v" }] }]
              },
              "futureField": "ignored"
            }
          ]
        }
    """.trimIndent()

    @Test
    fun parsesTheModel() {
        val model = ProjectModel.parse(JsonParser.parseString(json).asJsonObject)
        assertEquals(Paths.get("/p/target/lsp/kotlin-server.log"), model.logFile)
        assertEquals(21, model.jdk?.version)
        assertEquals("2.4.21", model.kotlinVersion)
        assertEquals("aa", model.library("g:a:1")?.sha256)
        val module = model.module("app.jvm.main")!!
        assertEquals(listOf("lib.jvm.main"), module.moduleIds)
        assertEquals(listOf("g:a:1"), module.libraryIds)
        assertEquals(listOf(Paths.get("/p/src"), Paths.get("/p/src-jvm")), module.sourceRoots)
        assertEquals("2.2", module.compiler.languageVersion)
        assertNull(module.compiler.apiVersion)
        assertEquals(listOf("k" to "v"), module.compiler.plugins.single().options)
    }

    @Test
    fun rejectsAnotherModelVersion() {
        val error = assertFailsWith<IllegalArgumentException> {
            ProjectModel.parse(JsonParser.parseString("""{ "modelVersion": 2 }""").asJsonObject)
        }
        assertTrue(error.message!!.contains("version 2"), error.message)
    }
}
