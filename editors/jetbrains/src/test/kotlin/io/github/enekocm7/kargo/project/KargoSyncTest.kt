package io.github.enekocm7.kargo.project

import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.process.CapturingProcessHandler
import com.intellij.lang.annotation.HighlightSeverity
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.progress.EmptyProgressIndicator
import com.intellij.openapi.projectRoots.ProjectJdkTable
import com.intellij.openapi.projectRoots.Sdk
import com.intellij.openapi.roots.ModuleRootManager
import com.intellij.openapi.roots.libraries.LibraryTablesRegistrar
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.testFramework.UsefulTestCase
import com.intellij.testFramework.fixtures.CodeInsightTestFixture
import com.intellij.testFramework.fixtures.IdeaTestFixtureFactory
import io.github.enekocm7.kargo.Kargo
import org.jetbrains.kotlin.idea.facet.KotlinFacet
import org.junit.Assume
import java.io.File
import java.nio.file.Files

/**
 * The whole import with a real `kargo`: runs `kargo init` and `kargo metadata` in a project and checks the model
 * that results. Skipped when `kargo` isn't installed or can't resolve the new project's dependencies (it needs
 * a populated cache or the network).
 */
class KargoSyncTest : UsefulTestCase() {
    private lateinit var fixture: CodeInsightTestFixture
    private lateinit var projectDir: File
    private var jdksBefore: List<Sdk> = emptyList()

    override fun setUp() {
        super.setUp()
        projectDir = Files.createTempDirectory("kargo-sync").toFile()
        val factory = IdeaTestFixtureFactory.getFixtureFactory()
        fixture = factory.createCodeInsightFixture(factory.createFixtureBuilder("sync", projectDir.toPath(), true).fixture)
        fixture.setUp()
        jdksBefore = ProjectJdkTable.getInstance().allJdks.toList()
    }

    override fun tearDown() {
        try {
            WriteAction.runAndWait<RuntimeException> {
                val table = ProjectJdkTable.getInstance()
                table.allJdks.filter { it !in jdksBefore }.forEach { table.removeJdk(it) }
            }
            fixture.tearDown()
            projectDir.deleteRecursively()
        } finally {
            super.tearDown()
        }
    }

    companion object {
        /** The IDE under test ships Kotlin 2.2, which can read libraries up to Kotlin 2.3 and no newer. */
        private const val KOTLIN = "2.2.20"
    }

    fun `test a project made by kargo init is imported`() {
        val kargo = Kargo.executable()
        Assume.assumeNotNull(kargo)
        val project = fixture.project
        val dir = File(project.basePath!!).also { it.mkdirs() }
        val init = CapturingProcessHandler(GeneralCommandLine(kargo!!.path, "init", "--no-vcs", "--kotlin", KOTLIN).withWorkDirectory(dir)).runProcess(60_000)
        Assume.assumeTrue("kargo init: ${init.stderr}", init.exitCode == 0)
        val metadata = CapturingProcessHandler(GeneralCommandLine(kargo.path, "metadata", "-q").withWorkDirectory(dir)).runProcess(300_000)
        Assume.assumeTrue("kargo metadata: ${metadata.stderr}", metadata.exitCode == 0)
        LocalFileSystem.getInstance().refreshAndFindFileByIoFile(dir)!!.refresh(false, true)

        val sync = KargoProjectSync.getInstance(project)
        assertTrue(sync.importOnce(force = false, indicator = EmptyProgressIndicator()))

        val module = ModuleManager.getInstance(project).findModuleByName(dir.name)
        assertNotNull(ModuleManager.getInstance(project).modules.joinToString { it.name }, module)
        assertNotNull(KotlinFacet.get(module!!))
        assertTrue(ModuleRootManager.getInstance(module).sourceRoots.any { it.name == "src" })
        val libraries = LibraryTablesRegistrar.getInstance().getLibraryTable(project).libraries.mapNotNull { it.name }
        assertTrue(libraries.toString(), libraries.any { it.startsWith("Kargo: org.jetbrains.kotlin:kotlin-stdlib:") })

        // The imported libraries resolve in the editor: println comes from the kotlin-stdlib jar in Kargo's cache.
        val main = LocalFileSystem.getInstance().refreshAndFindFileByIoFile(File(dir, "src/Main.kt"))!!
        fixture.configureFromExistingVirtualFile(main)
        val errors = fixture.doHighlighting().filter { it.severity == HighlightSeverity.ERROR }.map { it.description }
        assertEquals(emptyList<String>(), errors)

        // Nothing changed, so a second import has nothing to do.
        assertFalse(sync.importOnce(force = false, indicator = EmptyProgressIndicator()))
        // Reloading imports again.
        assertTrue(sync.importOnce(force = true, indicator = EmptyProgressIndicator()))
    }
}
