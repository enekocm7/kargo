package io.github.enekocm7.kargo.project

import com.google.gson.GsonBuilder
import com.intellij.codeInsight.daemon.impl.HighlightInfo
import com.intellij.lang.annotation.HighlightSeverity
import com.intellij.openapi.application.PathManager
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.application.runReadAction
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.projectRoots.ProjectJdkTable
import com.intellij.openapi.projectRoots.Sdk
import com.intellij.openapi.roots.DependencyScope
import com.intellij.openapi.roots.LibraryOrderEntry
import com.intellij.openapi.roots.ModuleOrderEntry
import com.intellij.openapi.roots.ModuleRootManager
import com.intellij.openapi.roots.libraries.LibraryTablesRegistrar
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.VfsUtilCore
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.testFramework.UsefulTestCase
import com.intellij.testFramework.fixtures.CodeInsightTestFixture
import com.intellij.testFramework.fixtures.IdeaTestFixtureFactory
import org.jetbrains.kotlin.idea.base.platforms.KotlinNativeLibraryKind
import org.jetbrains.kotlin.idea.facet.KotlinFacet
import org.jetbrains.kotlin.platform.jvm.isJvm
import org.jetbrains.kotlin.platform.konan.isNative
import org.junit.Assume
import java.io.File
import java.nio.file.Files
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

/**
 * Imports `kargo metadata`-shaped projects into a real IDE project and asks the Kotlin plugin to analyze code
 * against them: the part of the import that no unit test of the plan can show.
 */
class KargoImportTest : UsefulTestCase() {
    private lateinit var root: File
    private var jdksBefore: List<Sdk> = emptyList()
    private lateinit var myFixture: CodeInsightTestFixture
    private val project get() = myFixture.project

    override fun setUp() {
        super.setUp()
        val factory = IdeaTestFixtureFactory.getFixtureFactory()
        myFixture = factory.createCodeInsightFixture(factory.createFixtureBuilder(name).fixture)
        myFixture.setUp()
        jdksBefore = ProjectJdkTable.getInstance().allJdks.toList()
        root = Files.createTempDirectory("kargo-import").toFile()
    }

    override fun tearDown() {
        try {
            // The importer adds the project's JDK to the IDE's JDK table; the test framework wants it gone.
            WriteAction.runAndWait<RuntimeException> {
                ProjectJdkTable.getInstance().allJdks.filter { it !in jdksBefore }.forEach { ProjectJdkTable.getInstance().removeJdk(it) }
            }
            myFixture.tearDown()
            root.deleteRecursively()
        } finally {
            super.tearDown()
        }
    }

    // --- fixtures ---------------------------------------------------------------------------------------------

    private val ideStdlib = File(PathManager.getHomePath(), "plugins/Kotlin/kotlinc/lib/kotlin-stdlib.jar")

    private fun write(path: String, text: String): File = File(root, path).also {
        it.parentFile.mkdirs()
        it.writeText(text)
    }

    private fun json(builder: MutableMap<String, Any?>.() -> Unit): String {
        val map = linkedMapOf<String, Any?>("version" to 1, "root" to root.path, "resolved" to true)
        map.builder()
        return GsonBuilder().create().toJson(map)
    }

    private fun jvmTarget(pkg: String, dependencies: List<Map<String, Any?>>, extraRoots: Boolean = true) = linkedMapOf<String, Any?>(
        "kind" to "jvm",
        "source-roots" to listOf("$root/$pkg/src") + if (extraRoots) listOf("$root/$pkg/src-jvm") else emptyList(),
        "test-roots" to listOf("$root/$pkg/tests"),
        "resource-roots" to listOf("$root/$pkg/resources"),
        "test-resource-roots" to emptyList<String>(),
        "classes-dir" to "$root/$pkg/target/debug/jvm/classes",
        "jvm-target" to "21",
        "compiler-args" to emptyList<String>(),
        "compiler-plugins" to emptyList<Any>(),
        "dependencies" to listOf(linkedMapOf("platform" to "jvm", "compile" to dependencies, "runtime" to dependencies, "test" to dependencies)),
    )

    private fun nativeTarget(pkg: String, dependencies: List<Map<String, Any?>>, platform: String = "mingw_x64") = linkedMapOf<String, Any?>(
        "kind" to "native",
        "platforms" to listOf(platform),
        "source-roots" to listOf("$root/$pkg/src", "$root/$pkg/src-native"),
        "test-roots" to listOf("$root/$pkg/tests"),
        "compiler-args" to emptyList<String>(),
        "compiler-plugins" to emptyList<Any>(),
        "dependencies" to listOf(linkedMapOf("platform" to "native-$platform", "compile" to dependencies, "runtime" to dependencies, "test" to dependencies)),
    )

    private fun pkg(name: String, vararg targets: Map<String, Any?>) = linkedMapOf<String, Any?>(
        "name" to name,
        "member" to true,
        "root" to "$root/$name",
        "manifest" to "$root/$name/kargo.toml",
        "kotlin" to linkedMapOf("opt-in" to emptyList<String>()),
        "targets" to targets.toList(),
    )

    private fun artifact(module: String, version: String, file: File) =
        mapOf("module" to module, "version" to version, "path" to file.path, "sources" to null)

    private fun importMetadata(json: String, expectedFailures: List<String> = emptyList()): ImportPlan {
        val plan = KargoImportPlanner.plan(KargoMetadata.parse(json))
        val result = WriteAction.computeAndWait<KargoProjectImporter.Result, RuntimeException> { KargoProjectImporter.apply(project, plan) }
        assertEquals(expectedFailures, result.failures)
        return plan
    }

    /** A module the user made, as the IDE makes one for a folder: with the folder as its content root. */
    private fun userModule(name: String, contentRoot: File) {
        WriteAction.runAndWait<RuntimeException> {
            val model = ModuleManager.getInstance(project).getModifiableModel()
            val module = model.newModule("${root.path}/$name.iml", "JAVA_MODULE")
            val roots = ModuleRootManager.getInstance(module).modifiableModel
            roots.addContentEntry(VfsUtilCore.pathToUrl(contentRoot.invariantSeparatorsPath))
            roots.commit()
            model.commit()
        }
    }

    private fun virtualFile(file: File): VirtualFile = LocalFileSystem.getInstance().refreshAndFindFileByIoFile(file)!!

    private fun errors(file: File): List<String> {
        myFixture.configureFromExistingVirtualFile(virtualFile(file))
        return myFixture.doHighlighting().filter { it.severity == HighlightSeverity.ERROR }.map(HighlightInfo::getDescription)
    }

    /** A Kotlin/Native distribution on this machine whose klibs the IDE can read, or null. */
    private fun konan(): File? {
        val dirs = listOfNotNull(System.getenv("KONAN_DATA_DIR"), File(System.getProperty("user.home"), ".konan").path)
        return dirs.map(::File).flatMap { it.listFiles { f -> f.name.startsWith("kotlin-native-prebuilt") }.orEmpty().toList() }
            .filter { File(it, "klib/common/stdlib").isDirectory }
            .firstOrNull { distribution ->
                val abi = File(distribution, "klib/common/stdlib/default/manifest").takeIf { it.isFile }?.readLines()?.firstOrNull { it.startsWith("abi_version=") }
                // The IDE reads klibs up to the ABI of its own Kotlin plugin: 2.3 for 2025.2.
                abi != null && abi.substringAfter('=').substringBeforeLast('.') <= "2.3"
            }
    }

    // --- JVM --------------------------------------------------------------------------------------------------

    fun `test a JVM package resolves its libraries and the standard library`() {
        val main = write("app/src/Main.kt", "fun main() {\n    val l = listOf(1, 2)\n    println(l)\n}\n")
        write("app/kargo.toml", "")
        importMetadata(
            json {
                put("toolchain", mapOf("kotlin" to "2.4.21", "jdk" to mapOf("home" to System.getProperty("java.home"))))
                put("packages", listOf(pkg("app", jvmTarget("app", listOf(artifact("org.jetbrains.kotlin:kotlin-stdlib", "2.4.21", ideStdlib))))))
            },
        )
        val module = ModuleManager.getInstance(project).findModuleByName("app")!!
        val facet = KotlinFacet.get(module)!!
        assertTrue(facet.configuration.settings.targetPlatform!!.isJvm())
        assertEquals(listOf("src"), ModuleRootManager.getInstance(module).sourceRoots.map { it.name })
        assertEquals(emptyList<String>(), errors(main))
    }

    fun `test a library that is not on the classpath does not resolve`() {
        val main = write("app/src/Main.kt", "fun main() {\n    val l = listOf(1, 2)\n}\n")
        importMetadata(json { put("packages", listOf(pkg("app", jvmTarget("app", emptyList())))) })
        assertTrue(errors(main).toString(), errors(main).any { it.contains("listOf") })
    }

    fun `test libraries follow the metadata`() {
        write("app/src/Main.kt", "fun main() {}\n")
        val first = json { put("packages", listOf(pkg("app", jvmTarget("app", listOf(artifact("g:a", "1", ideStdlib), artifact("g:b", "2", ideStdlib)))))) }
        importMetadata(first)
        val table = LibraryTablesRegistrar.getInstance().getLibraryTable(project)
        assertEquals(setOf("Kargo: g:a:1", "Kargo: g:b:2"), table.libraries.mapNotNull { it.name }.toSet())

        importMetadata(json { put("packages", listOf(pkg("app", jvmTarget("app", listOf(artifact("g:a", "1", ideStdlib)))))) })
        assertEquals(setOf("Kargo: g:a:1"), table.libraries.mapNotNull { it.name }.toSet())
        val entries = ModuleRootManager.getInstance(ModuleManager.getInstance(project).findModuleByName("app")!!).orderEntries.filterIsInstance<LibraryOrderEntry>()
        assertEquals(listOf("Kargo: g:a:1"), entries.map { it.libraryName })

        importMetadata(json { put("packages", emptyList<Any>()) })
        assertNull(ModuleManager.getInstance(project).findModuleByName("app"))
        assertEquals(emptyList<String>(), table.libraries.mapNotNull { it.name })
    }

    fun `test libraries the user made are left alone`() {
        write("app/src/Main.kt", "fun main() {}\n")
        val table = LibraryTablesRegistrar.getInstance().getLibraryTable(project)
        WriteAction.runAndWait<RuntimeException> {
            val model = table.modifiableModel
            model.createLibrary("mine")
            model.commit()
        }
        importMetadata(json { put("packages", listOf(pkg("app", jvmTarget("app", emptyList())))) })
        importMetadata(json { put("packages", emptyList<Any>()) })
        assertNotNull(table.getLibraryByName("mine"))
        WriteAction.runAndWait<RuntimeException> {
            val model = table.modifiableModel
            model.removeLibrary(table.getLibraryByName("mine")!!)
            model.commit()
        }
    }

    fun `test a module the IDE made for the package is taken over`() {
        write("app/src/Main.kt", "fun main() {}\n")
        userModule("app", File(root, "app"))
        importMetadata(json { put("packages", listOf(pkg("app", jvmTarget("app", emptyList())))) })
        val module = ModuleManager.getInstance(project).findModuleByName("app")!!
        assertNotNull(KotlinFacet.get(module))
        assertTrue(ModuleRootManager.getInstance(module).sourceRoots.any { it.name == "src" })
        assertEquals(1, ModuleRootManager.getInstance(module).contentRoots.size)
        // It is Kargo's now, so it goes with the package.
        importMetadata(json { put("packages", emptyList<Any>()) })
        assertNull(ModuleManager.getInstance(project).findModuleByName("app"))
    }

    fun `test a module of the same name with other content is left alone`() {
        write("app/src/Main.kt", "fun main() {}\n")
        val elsewhere = File(root, "somewhere-else").also { it.mkdirs() }
        userModule("app", elsewhere)
        importMetadata(
            json { put("packages", listOf(pkg("app", jvmTarget("app", emptyList())))) },
            expectedFailures = listOf("The module app already exists and is not Kargo's; rename it to import the package"),
        )
        val module = ModuleManager.getInstance(project).findModuleByName("app")!!
        assertNull(KotlinFacet.get(module))
        assertEquals(listOf(virtualFile(elsewhere)), ModuleRootManager.getInstance(module).contentRoots.toList())
    }

    fun `test path dependencies are module dependencies and test libraries are test scoped`() {
        write("app/src/Main.kt", "fun main() {}\n")
        write("lib/src/Lib.kt", "package lib\nfun greet() = \"hi\"\n")
        val libTarget = jvmTarget("lib", emptyList(), extraRoots = false)
        val appDeps = listOf(mapOf("package" to "lib", "scope" to "compile"))
        val appTarget = jvmTarget("app", appDeps)
        @Suppress("UNCHECKED_CAST")
        val test = ((appTarget["dependencies"] as List<MutableMap<String, Any?>>).single())
        test["test"] = appDeps + artifact("org.junit:junit", "4", ideStdlib)
        importMetadata(json { put("packages", listOf(pkg("app", appTarget), pkg("lib", libTarget))) })

        val modules = ModuleManager.getInstance(project)
        val app = ModuleRootManager.getInstance(modules.findModuleByName("app")!!)
        val moduleDeps = app.orderEntries.filterIsInstance<ModuleOrderEntry>()
        assertEquals(listOf("lib"), moduleDeps.map { it.moduleName })
        assertEquals(DependencyScope.COMPILE, moduleDeps.single().scope)
        val library = app.orderEntries.filterIsInstance<LibraryOrderEntry>().single()
        assertEquals(DependencyScope.TEST, library.scope)
    }

    // --- Kotlin/Native ----------------------------------------------------------------------------------------

    fun `test a Native package resolves klibs`() {
        val konan = konan()
        Assume.assumeNotNull(konan)
        konan!!
        // Real Kargo klibs are zip files; pack one to check that the archive form resolves, too.
        val klib = File(root, "libs/stdlib.klib")
        zip(File(konan, "klib/common/stdlib"), klib)
        val main = write("tes/src/Main.kt", "import kotlinx.cinterop.CPointer\n\nfun main() {\n    val l = listOf(1)\n    println(l)\n}\n")
        write("tes/kargo.toml", "")
        val dependencies = listOf(artifact("org.jetbrains.kotlin:kotlin-stdlib-native", "2.3.20", klib))
        val plan = importMetadata(
            json {
                put("toolchain", mapOf("kotlin" to "2.3.20", "kotlin-native" to mapOf("version" to "2.3.20", "home" to konan.path)))
                put("packages", listOf(pkg("tes", nativeTarget("tes", dependencies))))
            },
        )
        val module = ModuleManager.getInstance(project).findModuleByName("tes")!!
        assertTrue(KotlinFacet.get(module)!!.configuration.settings.targetPlatform!!.isNative())
        val table = LibraryTablesRegistrar.getInstance().getLibraryTable(project)
        assertSame(KotlinNativeLibraryKind, (table.getLibraryByName("Kargo: org.jetbrains.kotlin:kotlin-stdlib-native:2.3.20") as com.intellij.openapi.roots.impl.libraries.LibraryEx).kind)
        assertTrue(plan.libraries.size > 2)
        // Both the zipped klib and the distribution's own stdlib are on the classpath; the build resolves
        // the same declarations from either.
        assertEquals(emptyList<String>(), errors(main).filter { !it.contains("OPT_IN") && !it.contains("opt-in") })
    }

    private fun zip(directory: File, target: File) {
        target.parentFile.mkdirs()
        ZipOutputStream(target.outputStream()).use { zip ->
            directory.walkTopDown().filter { it.isFile }.forEach { file ->
                zip.putNextEntry(ZipEntry(file.relativeTo(directory).invariantSeparatorsPath))
                file.inputStream().use { it.copyTo(zip) }
                zip.closeEntry()
            }
        }
    }

    // --- Multiplatform ----------------------------------------------------------------------------------------

    fun `test expect and actual declarations find each other across a common module`() {
        val konan = konan()
        Assume.assumeNotNull(konan)
        konan!!
        val common = write("hello/src/Platform.kt", "package hello\n\nexpect fun platformName(): String\n\nfun greeting(): String = \"Hello from \" + platformName()\n")
        val jvm = write("hello/src-jvm/Platform.kt", "package hello\n\nactual fun platformName(): String = \"the JVM\"\n")
        val native = write("hello/src-native/Platform.kt", "package hello\n\nactual fun platformName(): String = \"Native\"\n")
        write("hello/kargo.toml", "")
        val klib = File(root, "libs/stdlib.klib")
        zip(File(konan, "klib/common/stdlib"), klib)
        importMetadata(
            json {
                put("toolchain", mapOf("kotlin" to "2.3.20", "jdk" to mapOf("home" to System.getProperty("java.home")), "kotlin-native" to mapOf("version" to "2.3.20", "home" to konan.path)))
                put(
                    "packages",
                    listOf(
                        pkg(
                            "hello",
                            jvmTarget("hello", listOf(artifact("org.jetbrains.kotlin:kotlin-stdlib", "2.3.20", ideStdlib))),
                            nativeTarget("hello", emptyList()),
                        ),
                    ),
                )
            },
        )
        val modules = ModuleManager.getInstance(project).modules.map { it.name }.filter { it.startsWith("hello") }.sorted()
        assertEquals(listOf("hello.common", "hello.jvm", "hello.native"), modules)
        assertEquals(emptyList<String>(), errors(common))
        assertEquals(emptyList<String>(), errors(jvm))
        assertEquals(emptyList<String>(), errors(native))
    }
}
