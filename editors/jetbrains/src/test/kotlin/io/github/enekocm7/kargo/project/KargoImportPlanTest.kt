package io.github.enekocm7.kargo.project

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class KargoImportPlanTest {
    /** The files of the fixtures: every directory exists, and the Native distribution has two platform libraries. */
    private object Files : PlanFiles {
        override fun exists(path: String) = !path.endsWith("src-android") && !path.endsWith("/w/app/test-resources")

        override fun subdirectories(path: String) =
            if (path == "/konan/2.4.21/klib/platform/mingw_x64") listOf("org.jetbrains.kotlin.native.platform.posix", "org.jetbrains.kotlin.native.platform.windows") else emptyList()
    }

    private fun plan(fixture: String): ImportPlan {
        val json = javaClass.getResource("/metadata/$fixture.json")!!.readText()
        return KargoImportPlanner.plan(KargoMetadata.parse(json), Files)
    }

    @Test
    fun `metadata is read`() {
        val metadata = KargoMetadata.parse(javaClass.getResource("/metadata/jvm.json")!!.readText())
        assertEquals(1, metadata.version)
        assertEquals("/jdk/21", metadata.jdkHome)
        assertEquals(listOf("/p/app/kargo.toml", "/p/app/kargo.lock"), metadata.watch)
        val target = metadata.packages.single().targets.single()
        assertEquals("jvm", target.kind)
        assertEquals("21", target.jvmTarget)
        assertEquals(1, target.dependencies!!.size)
        assertEquals("/cache/kotlin-stdlib-2.4.21-sources.jar", target.dependencies!!.single().compile.first().sources)
        assertNull(target.dependencies!!.single().compile.last().sources)
    }

    @Test
    fun `a JVM package is one module with its roots`() {
        val module = plan("jvm").modules.single()
        assertEquals("app", module.name)
        assertEquals(PlatformKind.JVM, module.platform)
        assertEquals("/jdk/21", module.jdkHome)
        val content = module.contents.single()
        assertEquals("/p/app", content.root)
        assertEquals(listOf("/p/app/target"), content.excluded)
        assertEquals(
            listOf(
                "SOURCE /p/app/src", "SOURCE /p/app/src-jvm", "TEST /p/app/tests", "TEST /p/app/tests-jvm",
                "RESOURCE /p/app/resources", "TEST_RESOURCE /p/app/test-resources", "TEST /p/app/examples",
            ),
            content.roots.map { it.toString() },
        )
        assertEquals("/p/app/target/debug/jvm/classes", module.classesDir)
        assertEquals("/p/app/target/debug/jvm/test-classes", module.testClassesDir)
    }

    @Test
    fun `libraries are shared by module and version with their sources`() {
        val plan = plan("jvm")
        val stdlib = plan.libraries.single { it.name == "Kargo: org.jetbrains.kotlin:kotlin-stdlib:2.4.21" }
        assertEquals(LibraryKind.JVM, stdlib.kind)
        assertEquals(listOf("/cache/kotlin-stdlib-2.4.21.jar"), stdlib.classes)
        assertEquals(listOf("/cache/kotlin-stdlib-2.4.21-sources.jar"), stdlib.sources)
        assertEquals(4, plan.libraries.size)
    }

    @Test
    fun `a library is compile, runtime or test scoped by the classpath it is on`() {
        val scopes = plan("jvm").modules.single().dependencies.associate { it.library to it.scope }
        assertEquals(DependencyScope.COMPILE, scopes["Kargo: org.jetbrains.kotlin:kotlin-stdlib:2.4.21"])
        assertEquals(DependencyScope.COMPILE, scopes["Kargo: g:lib:1.0"])
        assertEquals(DependencyScope.RUNTIME, scopes["Kargo: g:driver:3.0"])
        assertEquals(DependencyScope.TEST, scopes["Kargo: org.junit.jupiter:junit-jupiter:5.11.0"])
    }

    @Test
    fun `compiler settings come from the manifest`() {
        val module = plan("jvm").modules.single()
        assertEquals("2.3", module.languageVersion)
        assertNull(module.apiVersion)
        assertEquals(listOf("kotlin.ExperimentalStdlibApi"), module.optIn)
        assertEquals(listOf("-Xcontext-parameters"), module.extraArgs)
        assertEquals(listOf("/cache/serialization-plugin.jar"), module.pluginClasspath)
        assertEquals("21", module.jvmTarget)
    }

    @Test
    fun `a Native package depends on the klibs and the Kotlin Native distribution`() {
        val plan = plan("native")
        val module = plan.modules.single()
        assertEquals("tes", module.name)
        assertEquals(PlatformKind.NATIVE, module.platform)
        assertEquals("mingw_x64", module.nativeTarget)
        assertNull("a Native module needs no JDK", module.jdkHome)

        val raylib = plan.libraries.single { it.name == "Kargo: io.github.enekocm7:raylib-kt-mingwx64:0.3.0" }
        assertEquals(LibraryKind.NATIVE, raylib.kind)
        assertEquals(listOf("/cache/raylib-kt-mingwx64-0.3.0.klib", "/cache/raylib-kt-mingwx64-0.3.0-cinterop-raylib.klib"), raylib.classes)

        val names = plan.libraries.map { it.name }
        assertTrue(names.toString(), "Kargo: Kotlin/Native 2.4.21 stdlib" in names)
        assertTrue(names.toString(), "Kargo: Kotlin/Native 2.4.21 mingw_x64 posix" in names)
        assertTrue(names.toString(), "Kargo: Kotlin/Native 2.4.21 mingw_x64 windows" in names)
        assertEquals(listOf("/konan/2.4.21/klib/common/stdlib"), plan.libraries.single { it.name == "Kargo: Kotlin/Native 2.4.21 stdlib" }.classes)
        assertTrue(module.dependencies.all { it.scope == DependencyScope.COMPILE })
        assertEquals(names.toSet(), module.dependencies.mapNotNull { it.library }.toSet())
    }

    @Test
    fun `a package with several targets gets a common module and one module per target`() {
        val modules = plan("workspace").modules.associateBy { it.name }
        // Android is Gradle's; lib has a JVM and a Native target too.
        assertEquals(setOf("app.common", "app.jvm", "app.native", "lib.common", "lib.jvm", "lib.native"), modules.keys)

        val common = modules.getValue("app.common")
        assertEquals(PlatformKind.JVM, common.platform)
        assertEquals(listOf("SOURCE /w/app/src", "TEST /w/app/tests", "TEST /w/app/examples"), common.contents.single().roots.map { it.toString() })
        assertTrue(common.dependsOn.isEmpty())

        val jvm = modules.getValue("app.jvm")
        assertEquals(listOf("app.common"), jvm.dependsOn)
        assertEquals(
            listOf("SOURCE /w/app/src-jvm", "TEST /w/app/tests-jvm", "RESOURCE /w/app/resources"),
            jvm.contents.flatMap { it.roots }.map { it.toString() },
            // test-resources doesn't exist, so it is left out.
        )
        val native = modules.getValue("app.native")
        assertEquals(PlatformKind.NATIVE, native.platform)
        assertEquals("linux_x64", native.nativeTarget)
        assertEquals(listOf("app.common"), native.dependsOn)
    }

    @Test
    fun `a path dependency becomes a dependency on the module of the same target`() {
        val modules = plan("workspace").modules.associateBy { it.name }
        assertEquals(
            listOf("lib.jvm"),
            modules.getValue("app.jvm").dependencies.mapNotNull { it.module },
        )
        assertEquals(listOf("lib.native"), modules.getValue("app.native").dependencies.mapNotNull { it.module })
        // The common module follows the first target.
        assertEquals(listOf("lib.jvm"), modules.getValue("app.common").dependencies.mapNotNull { it.module })
    }

    @Test
    fun `target modules depend on the libraries of their own target`() {
        val plan = plan("workspace")
        val jvm = plan.modules.single { it.name == "app.jvm" }
        assertEquals(listOf("Kargo: org.jetbrains.kotlin:kotlin-stdlib:2.4.21"), jvm.dependencies.mapNotNull { it.library })
        assertEquals(DependencyScope.COMPILE, jvm.dependencies.single { it.library != null }.scope)
        val native = plan.modules.single { it.name == "app.native" }
        assertFalse(native.dependencies.any { it.library?.contains("kotlin-stdlib") == true })
    }

    @Test
    fun `a Wasm package uses the Wasm platform and klibs`() {
        val metadata = KargoMetadata.parse(
            """{"version":1,"root":"/a","packages":[{"name":"a","root":"/a","targets":[{"kind":"wasm","source-roots":["/a/src","/a/src-wasm"],"test-roots":[],
            "dependencies":[{"platform":"wasm-wasi","compile":[{"module":"org.jetbrains.kotlin:kotlin-stdlib-wasm-wasi","version":"2.4.21","path":"/cache/stdlib-wasm-wasi.klib"}],"runtime":[],"test":[]}]}]}]}""",
        )
        val plan = KargoImportPlanner.plan(metadata, Files)
        assertEquals(PlatformKind.WASM_WASI, plan.modules.single().platform)
        assertEquals(LibraryKind.WASM_WASI, plan.libraries.single().kind)
    }

    @Test
    fun `a Wasm browser app describes the browser APIs and its installed npm packages`() {
        val installed = setOf("/out/node_modules/@js-joda/core", "/out/test/node_modules/mocha")
        val onDisk = object : PlanFiles {
            override fun exists(path: String) = path in installed

            override fun subdirectories(path: String) = emptyList<String>()
        }
        val metadata = KargoMetadata.parse(
            """{"version":1,"root":"/a","packages":[{"name":"a","root":"/a","targets":[{"kind":"wasm","source-roots":["/a/src"],"test-roots":["/a/tests"],
            "dependencies":[{"platform":"wasm-js","compile":[
              {"module":"org.jetbrains.kotlin:kotlin-stdlib-wasm-js","version":"2.4.21","path":"/cache/stdlib-wasm-js.klib"},
              {"module":"org.jetbrains.kotlinx:kotlinx-browser-wasm-js","version":"0.5.0","path":"/cache/browser-wasm-js.klib"}],"runtime":[],"test":[]}],
            "npm":[{"name":"@js-joda/core","version":"3.2.0","directory":"/out/node_modules/@js-joda/core","test":false},
                   {"name":"date-fns","version":"4.1.0","directory":"/out/node_modules/date-fns","test":false},
                   {"name":"mocha","version":"10.0.0","directory":"/out/test/node_modules/mocha","test":true}]}]}]}""",
        )
        val plan = KargoImportPlanner.plan(metadata, onDisk)
        val module = plan.modules.single()
        assertEquals(PlatformKind.WASM_JS, module.platform)

        val browser = plan.libraries.single { it.name == "Kargo: org.jetbrains.kotlinx:kotlinx-browser-wasm-js:0.5.0" }
        assertEquals(LibraryKind.WASM_JS, browser.kind)
        assertTrue(module.dependencies.any { it.library == browser.name && it.scope == DependencyScope.COMPILE })

        // Installed packages are described, at the scope they are needed for; a missing one isn't.
        val joda = plan.libraries.single { it.name == "Kargo: npm @js-joda/core@3.2.0" }
        assertEquals(LibraryKind.NPM, joda.kind)
        assertEquals(listOf("/out/node_modules/@js-joda/core"), joda.classes)
        assertEquals(DependencyScope.RUNTIME, module.dependencies.single { it.library == joda.name }.scope)
        assertEquals(DependencyScope.TEST, module.dependencies.single { it.library == "Kargo: npm mocha@10.0.0" }.scope)
        assertFalse(plan.libraries.any { it.name.contains("date-fns") })
    }

    @Test
    fun `android only packages have no module`() {
        val metadata = KargoMetadata.parse(
            """{"version":1,"root":"/a","packages":[{"name":"a","root":"/a","targets":[{"kind":"android","source-roots":["/a/src"],"test-roots":[],"dependencies":null}]}]}""",
        )
        assertTrue(KargoImportPlanner.plan(metadata, Files).modules.isEmpty())
    }

    @Test
    fun `unresolved metadata still gives modules with roots`() {
        val metadata = KargoMetadata.parse(
            """{"version":1,"root":"/a","resolved":false,"packages":[{"name":"a","root":"/a","targets":[{"kind":"jvm","source-roots":["/a/src"],"test-roots":["/a/tests"],"dependencies":null}]}]}""",
        )
        val module = KargoImportPlanner.plan(metadata, Files).modules.single()
        assertNotNull(module.contents.single().roots.firstOrNull())
        assertTrue(module.dependencies.isEmpty())
    }
}
