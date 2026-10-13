package io.github.enekocm7.kargo.project

import com.intellij.openapi.application.runWriteAction
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.roots.DependencyScope
import com.intellij.openapi.roots.LibraryOrderEntry
import com.intellij.openapi.roots.ModuleOrderEntry
import com.intellij.openapi.roots.ModuleRootManager
import com.intellij.openapi.roots.OrderRootType
import com.intellij.openapi.util.io.FileUtil
import com.intellij.testFramework.HeavyPlatformTestCase
import java.io.File
import java.util.zip.ZipOutputStream

class KargoProjectImportTest : HeavyPlatformTestCase() {
    private lateinit var dir: File

    override fun setUp() {
        super.setUp()
        dir = FileUtil.createTempDirectory("kargo-import", null)
    }

    override fun tearDown() {
        try {
            FileUtil.delete(dir)
        } finally {
            super.tearDown()
        }
    }

    private fun jar(name: String) = File(dir, name).also { ZipOutputStream(it.outputStream()).close() }.path

    private fun module(name: String, vararg libraries: KargoLibrary, dependencies: List<Pair<String, Boolean>> = emptyList()): KargoModule {
        val root = File(dir, name).path
        return KargoModule(
            name = name,
            root = root,
            sourceRoots = listOf("$root/src", "$root/src-jvm"),
            testRoots = listOf("$root/tests"),
            resourceRoots = listOf("$root/resources"),
            testResourceRoots = emptyList(),
            excluded = listOf(File(dir, "target").path),
            libraries = libraries.toList(),
            moduleDependencies = dependencies,
            target = "jvm",
        )
    }

    fun `test packages become modules with roots, libraries and dependencies`() {
        val stdlib = KargoLibrary("Kargo: kotlin-stdlib:2.4.21", jar("stdlib.jar"), jar("stdlib-sources.jar"), false)
        val junit = KargoLibrary("Kargo: junit:5", jar("junit.jar"), null, true)
        val project = KargoProject(
            "f", null, listOf(module("app", stdlib, junit, dependencies = listOf("lib" to false)), module("lib", stdlib)),
        )
        runWriteAction { KargoProjectImport.apply(this.project, project) }

        val app = ModuleManager.getInstance(this.project).findModuleByName("app")!!
        val root = ModuleRootManager.getInstance(app)
        val content = root.contentEntries.single()
        val sources = content.sourceFolders.filter { !it.isTestSource }.map { it.url.substringAfterLast('/') }
        assertEquals(listOf("src", "src-jvm", "resources"), sources)
        assertTrue(content.sourceFolders.any { it.isTestSource && it.url.endsWith("/app/tests") })
        // `target` is outside the package here, so there is nothing to exclude.
        assertTrue(content.excludeFolderUrls.isEmpty())
        val libraries = root.orderEntries.filterIsInstance<LibraryOrderEntry>()
        assertEquals(listOf(DependencyScope.COMPILE, DependencyScope.TEST), libraries.map { it.scope })
        assertEquals(1, libraries[0].library!!.getUrls(OrderRootType.SOURCES).size)
        assertEquals(1, libraries[0].library!!.getUrls(OrderRootType.CLASSES).size)
        val dependency = root.orderEntries.filterIsInstance<ModuleOrderEntry>().single()
        assertEquals("lib", dependency.moduleName)

        // Importing again replaces what Kargo set up and drops packages that are gone.
        runWriteAction { KargoProjectImport.apply(this.project, KargoProject("g", null, listOf(module("app", stdlib)))) }
        val manager = ModuleManager.getInstance(this.project)
        assertNull(manager.findModuleByName("lib"))
        assertEquals(1, ModuleRootManager.getInstance(manager.findModuleByName("app")!!).orderEntries.filterIsInstance<LibraryOrderEntry>().size)
    }
}
