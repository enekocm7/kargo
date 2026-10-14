// Applies an ImportPlan to the IDE's project model: one module per planned module with its source roots,
// project libraries for the resolved artifacts and a Kotlin facet that sets the platform. It uses the
// long-standing module and library APIs, so a plan can be applied again and again: what Kargo manages
// (libraries named "Kargo: ...", the modules listed in KargoImportState) is brought up to date and what it
// no longer lists is removed. Modules and libraries a user made themselves are never touched.

package io.github.enekocm7.kargo.project

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.diagnostic.logger
import com.intellij.openapi.module.Module
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.projectRoots.JavaSdk
import com.intellij.openapi.projectRoots.ProjectJdkTable
import com.intellij.openapi.projectRoots.Sdk
import com.intellij.openapi.projectRoots.impl.SdkConfigurationUtil
import com.intellij.openapi.roots.CompilerModuleExtension
import com.intellij.openapi.roots.DependencyScope as IdeScope
import com.intellij.openapi.roots.ModifiableModelsProvider
import com.intellij.openapi.roots.ModifiableRootModel
import com.intellij.openapi.roots.ModuleRootManager
import com.intellij.openapi.roots.ModuleSourceOrderEntry
import com.intellij.openapi.roots.OrderRootType
import com.intellij.openapi.roots.impl.ModifiableModelCommitter
import com.intellij.openapi.roots.libraries.Library
import com.intellij.openapi.roots.libraries.LibraryTable
import com.intellij.openapi.roots.libraries.LibraryTablesRegistrar
import com.intellij.openapi.vfs.VfsUtilCore
import com.intellij.openapi.vfs.VirtualFileManager
import com.intellij.openapi.roots.impl.libraries.LibraryEx
import com.intellij.openapi.util.Disposer
import org.jetbrains.jps.model.java.JavaResourceRootType
import org.jetbrains.jps.model.java.JavaSourceRootType
import java.io.File

object KargoProjectImporter {
    /** The id of IntelliJ's Java module type, which Kotlin modules are too. */
    private const val JAVA_MODULE_TYPE = "JAVA_MODULE"

    private val log = logger<KargoProjectImporter>()

    /** What applying a plan did, for the logs and for tests. */
    class Result(val modules: List<String>, val failures: List<String>)

    /** Applies `plan` to `project`. Must run on the event thread under a write action. */
    fun apply(project: Project, plan: ImportPlan): Result {
        ApplicationManager.getApplication().assertWriteAccessAllowed()
        val failures = ArrayList<String>()
        val libraryTable = LibraryTablesRegistrar.getInstance().getLibraryTable(project)
        syncLibraries(libraryTable, plan.libraries)

        val moduleManager = ModuleManager.getInstance(project)
        val moduleModel = moduleManager.getModifiableModel()
        val wanted = plan.modules.map { it.name }.toSet()
        val state = KargoImportState.getInstance(project).state
        for (name in state.modules.toList()) {
            if (name !in wanted) moduleModel.findModuleByName(name)?.let(moduleModel::disposeModule)
        }
        val modules = HashMap<String, Module>()
        val specs = ArrayList<ModuleSpec>()
        for (spec in plan.modules) {
            val existing = moduleModel.findModuleByName(spec.name)
            if (existing != null && spec.name !in state.modules && isForeign(existing, spec)) {
                failures += "The module ${spec.name} already exists and is not Kargo's; rename it to import the package"
                continue
            }
            modules[spec.name] = existing ?: moduleModel.newModule(imlPath(project, spec.name), JAVA_MODULE_TYPE)
            specs += spec
        }
        val rootModels = ArrayList<ModifiableRootModel>()
        try {
            for (spec in specs) {
                val model = ModuleRootManager.getInstance(modules.getValue(spec.name)).modifiableModel
                rootModels += model
                fill(model, spec, libraryTable, modules)
            }
        } catch (e: Throwable) {
            // Nothing is committed: give the half-built models back.
            rootModels.forEach { it.dispose() }
            moduleModel.dispose()
            throw e
        }
        ModifiableModelCommitter.multiCommit(rootModels, moduleModel)
        state.modules = specs.map { it.name }.toMutableList()

        for (spec in specs) {
            try {
                KotlinSupport.configureFacet(modules.getValue(spec.name), spec)
            } catch (e: LinkageError) {
                // The IDE's Kotlin plugin changed in a way this build doesn't know.
                log.warn("couldn't configure Kotlin for ${spec.name}", e)
                failures += "Kotlin settings of ${spec.name}: ${e.message}"
            }
        }
        return Result(specs.map { it.name }, failures)
    }

    /** Whether `module`, which Kargo didn't create, has content outside the package: it is then someone else's. */
    private fun isForeign(module: Module, spec: ModuleSpec): Boolean {
        val root = VfsUtilCore.pathToUrl(spec.packageRoot)
        return ModuleRootManager.getInstance(module).contentRootUrls.any { it != root && !it.startsWith("$root/") }
    }

    private fun imlPath(project: Project, name: String): String = "${project.basePath ?: ""}/.idea/modules/$name.iml"

    private fun syncLibraries(table: LibraryTable, specs: List<LibrarySpec>) {
        val model = table.modifiableModel
        val wanted = specs.map { it.name }.toSet()
        for (library in model.libraries) {
            val name = library.name ?: continue
            if (name.startsWith(ImportPlan.LIBRARY_PREFIX) && name !in wanted) model.removeLibrary(library)
        }
        for (spec in specs) {
            val kind = KotlinSupport.libraryKind(spec.kind)
            val library = model.getLibraryByName(spec.name) ?: model.createLibrary(spec.name, kind)
            val editor = library.modifiableModel
            if (editor is LibraryEx.ModifiableModelEx && (library as LibraryEx).kind != kind) editor.kind = kind
            syncRoots(editor, OrderRootType.CLASSES, spec.classes.map(::rootUrl))
            syncRoots(editor, OrderRootType.SOURCES, spec.sources.map(::rootUrl))
            if (editor.isChanged) editor.commit() else Disposer.dispose(editor)
        }
        model.commit()
    }

    private fun syncRoots(editor: Library.ModifiableModel, type: OrderRootType, urls: List<String>) {
        if (editor.getUrls(type).toList() == urls) return
        for (url in editor.getUrls(type)) editor.removeRoot(url, type)
        for (url in urls) editor.addRoot(url, type)
    }

    /** `jar://...!/` for an archive (a jar, a sources jar, a klib), `file://...` for a directory (an unpacked klib). */
    fun rootUrl(path: String): String {
        val file = File(path)
        return if (file.isDirectory) VfsUtilCore.pathToUrl(norm(path)) else VirtualFileManager.constructUrl("jar", norm(path)) + "!/"
    }

    private fun fill(model: ModifiableRootModel, spec: ModuleSpec, libraries: LibraryTable, modules: Map<String, Module>) {
        for (entry in model.contentEntries) model.removeContentEntry(entry)
        for (entry in model.orderEntries) if (entry !is ModuleSourceOrderEntry) model.removeOrderEntry(entry)

        for (content in spec.contents) {
            val entry = model.addContentEntry(VfsUtilCore.pathToUrl(content.root))
            for (root in content.roots) {
                val url = VfsUtilCore.pathToUrl(root.path)
                when (root.kind) {
                    RootKind.SOURCE -> entry.addSourceFolder(url, JavaSourceRootType.SOURCE)
                    RootKind.TEST -> entry.addSourceFolder(url, JavaSourceRootType.TEST_SOURCE)
                    RootKind.RESOURCE -> entry.addSourceFolder(url, JavaResourceRootType.RESOURCE)
                    RootKind.TEST_RESOURCE -> entry.addSourceFolder(url, JavaResourceRootType.TEST_RESOURCE)
                }
            }
            for (excluded in content.excluded) entry.addExcludeFolder(VfsUtilCore.pathToUrl(excluded))
        }

        model.sdk = spec.jdkHome?.let(::findOrCreateJdk)
        val output = model.getModuleExtension(CompilerModuleExtension::class.java)
        if (spec.classesDir != null) {
            output.inheritCompilerOutputPath(false)
            output.setCompilerOutputPath(VfsUtilCore.pathToUrl(spec.classesDir))
            output.setCompilerOutputPathForTests(VfsUtilCore.pathToUrl(spec.testClassesDir ?: spec.classesDir))
            output.setExcludeOutput(true)
        }

        for (name in spec.dependsOn) {
            val module = modules[name] ?: continue
            model.addModuleOrderEntry(module).scope = IdeScope.COMPILE
        }
        for (dependency in spec.dependencies) {
            val scope = when (dependency.scope) {
                DependencyScope.COMPILE -> IdeScope.COMPILE
                DependencyScope.RUNTIME -> IdeScope.RUNTIME
                DependencyScope.TEST -> IdeScope.TEST
            }
            if (dependency.library != null) {
                val library = libraries.getLibraryByName(dependency.library) ?: continue
                model.addLibraryEntry(library).scope = scope
            } else if (dependency.module != null) {
                val module = modules[dependency.module] ?: continue
                if (module != model.module) model.addModuleOrderEntry(module).scope = scope
            }
        }
    }

    /** The IDE's JDK for the home Kargo uses, added to the IDE's JDK table when it isn't there yet. */
    private fun findOrCreateJdk(home: String): Sdk? {
        val normalized = norm(home)
        val table = ProjectJdkTable.getInstance()
        table.allJdks.firstOrNull { it.sdkType is JavaSdk && it.homePath?.let(::norm) == normalized }?.let { return it }
        if (!JavaSdk.getInstance().isValidSdkHome(home)) return null
        return SdkConfigurationUtil.createAndAddSDK(home, JavaSdk.getInstance())
    }
}
