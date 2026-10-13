package io.github.enekocm7.kargo.project

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class KargoModelTest {
    private val metadata = """
        {
          "fingerprint": "abc",
          "target-dir": "/p/target",
          "toolchain": { "jdk": { "request": "21", "home": "/jdk-21", "version": 21 } },
          "packages": [
            {
              "name": "app", "root": "/p/app",
              "targets": [
                { "kind": "wasm", "source-roots": ["/p/app/src", "/p/app/src-wasm"], "test-roots": [], "dependencies": [] },
                {
                  "kind": "jvm",
                  "source-roots": ["/p/app/src", "/p/app/src-jvm"],
                  "test-roots": ["/p/app/tests", "/p/app/tests-jvm"],
                  "resource-roots": ["/p/app/resources"],
                  "test-resource-roots": ["/p/app/test-resources"],
                  "dependencies": [{
                    "platform": "jvm",
                    "compile": [
                      { "package": "lib", "scope": "compile" },
                      { "module": "org.jetbrains.kotlin:kotlin-stdlib", "version": "2.4.21", "path": "/c/stdlib.jar", "sources": "/c/stdlib-sources.jar" },
                      { "module": "org.example:json", "version": "1.0", "path": "/c/json.jar", "sources": null }
                    ],
                    "runtime": [],
                    "test": [
                      { "package": "lib", "scope": "compile" },
                      { "package": "testlib", "scope": "compile" },
                      { "module": "org.jetbrains.kotlin:kotlin-stdlib", "version": "2.4.21", "path": "/c/stdlib.jar", "sources": "/c/stdlib-sources.jar" },
                      { "module": "org.junit:junit", "version": "5", "path": "/c/junit.jar", "sources": null }
                    ]
                  }]
                }
              ]
            },
            {
              "name": "wasmonly", "root": "/p/wasmonly",
              "targets": [{ "kind": "wasm", "source-roots": ["/p/wasmonly/src-wasm"], "test-roots": [], "dependencies": [{ "platform": "wasm-js", "compile": [{ "module": "a:b", "version": "1", "path": "/c/b.klib" }] }] }]
            },
            { "name": "droid", "root": "/p/droid", "targets": [{ "kind": "android", "source-roots": ["/p/droid/android"], "test-roots": [], "dependencies": null }] }
          ]
        }
    """

    @Test
    fun `a package takes the roots and classpath of its JVM target`() {
        val project = KargoProject.parse(metadata)
        assertEquals("abc", project.fingerprint)
        assertEquals("/jdk-21", project.jdkHome)
        val app = project.modules.first { it.name == "app" }
        assertEquals("jvm", app.target)
        assertEquals(listOf("/p/app/src", "/p/app/src-jvm"), app.sourceRoots)
        assertEquals(listOf("/p/app/tests", "/p/app/tests-jvm"), app.testRoots.take(2))
        assertTrue("examples are compiled against the sources", app.testRoots.last().endsWith("examples"))
        assertEquals(listOf("/p/app/resources"), app.resourceRoots)
        assertEquals(listOf("/p/app/test-resources"), app.testResourceRoots)
        assertEquals(listOf("/p/target"), app.excluded)
    }

    @Test
    fun `test-only libraries and packages are scoped to tests`() {
        val app = KargoProject.parse(metadata).modules.first { it.name == "app" }
        assertEquals(
            listOf("Kargo: org.jetbrains.kotlin:kotlin-stdlib:2.4.21" to false, "Kargo: org.example:json:1.0" to false, "Kargo: org.junit:junit:5" to true),
            app.libraries.map { it.name to it.test },
        )
        assertEquals("/c/stdlib-sources.jar", app.libraries.first().sources)
        assertNull(app.libraries[1].sources)
        assertEquals(listOf("lib" to false, "testlib" to true), app.moduleDependencies)
    }

    @Test
    fun `packages without a JVM target have roots but no libraries`() {
        val modules = KargoProject.parse(metadata).modules
        val wasm = modules.first { it.name == "wasmonly" }
        assertEquals("wasm", wasm.target)
        assertEquals(listOf("/p/wasmonly/src-wasm"), wasm.sourceRoots)
        assertTrue(wasm.libraries.isEmpty())
        val droid = modules.first { it.name == "droid" }
        assertEquals("android", droid.target)
        assertTrue(droid.libraries.isEmpty())
    }
}
