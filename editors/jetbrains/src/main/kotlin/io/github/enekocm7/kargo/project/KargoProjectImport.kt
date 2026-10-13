// Teaching the IDE about the project. Kargo projects have no Gradle or Maven files, so IntelliJ
// knows no source roots, JDK or libraries and its Kotlin support can't resolve anything. This
// describes the project to it from `kargo metadata`: one module per package, with the roots, the
// JDK and the resolved libraries (and their sources) of its JVM target.

package io.github.enekocm7.kargo.project

import com.intellij.execution.ExecutionListener
import com.intellij.execution.ExecutionManager
import com.intellij.execution.process.CapturingProcessHandler
import com.intellij.execution.process.ProcessHandler
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.ide.util.PropertiesComponent
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.diagnostic.logger
import com.intellij.openapi.module.Module
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import com.intellij.openapi.projectRoots.JavaSdk
import com.intellij.openapi.projectRoots.ProjectJdkTable
import com.intellij.openapi.projectRoots.Sdk
import com.intellij.openapi.roots.DependencyScope
import com.intellij.openapi.roots.ModifiableRootModel
import com.intellij.openapi.roots.ModuleRootManager
import com.intellij.openapi.roots.ModuleSourceOrderEntry
import com.intellij.openapi.roots.OrderRootType
import com.intellij.openapi.startup.ProjectActivity
import com.intellij.openapi.util.io.FileUtil
import com.intellij.openapi.vfs.VfsUtil
import com.intellij.openapi.vfs.VfsUtilCore
import com.intellij.openapi.vfs.VirtualFileManager
import com.intellij.openapi.vfs.newvfs.BulkFileListener
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import com.intellij.util.Alarm
import io.github.enekocm7.kargo.Kargo
import io.github.enekocm7.kargo.KargoNotifications
import io.github.enekocm7.kargo.run.KargoRunConfiguration
import org.jetbrains.jps.model.java.JavaResourceRootType
import org.jetbrains.jps.model.java.JavaSourceRootType
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean

/** Imports the project when it opens and again when a `kargo.toml` or `kargo.lock` changes. */
@Service(Service.Level.PROJECT)
class KargoProjectImport(private val project: Project) : Disposable {
    private val alarm = Alarm(Alarm.ThreadToUse.POOLED_THREAD, this)
    private val running = AtomicBoolean(false)
    private val again = AtomicBoolean(false)
    private val againForced = AtomicBoolean(false)

    /** Imports on open, then follows changes to the files the metadata depends on. */
    fun start() {
        val connection = project.messageBus.connect(this)
        connection.subscribe(VirtualFileManager.VFS_CHANGES, object : BulkFileListener {
            override fun after(events: List<VFileEvent>) {
                if (events.any { it.path.endsWith("/kargo.toml") || it.path.endsWith("/kargo.lock") }) schedule(force = false)
            }
        })
        // Sources jars come without a change to either file: `kargo fetch --sources` and the like.
        connection.subscribe(ExecutionManager.EXECUTION_TOPIC, object : ExecutionListener {
            override fun processTerminated(executorId: String, env: ExecutionEnvironment, handler: ProcessHandler, exitCode: Int) {
                val configuration = env.runProfile as? KargoRunConfiguration ?: return
                if (exitCode == 0 && configuration.subcommand in AFFECTS_PROJECT) schedule(force = configuration.subcommand == "fetch")
            }
        })
        schedule(force = false)
    }

    /** Imports soon: `force` even when `kargo.toml` and `kargo.lock` haven't changed, for example after sources were fetched. */
    fun schedule(force: Boolean) {
        alarm.cancelAllRequests()
        alarm.addRequest({ run(force) }, if (force) 0 else 1000)
    }

    private fun run(force: Boolean) {
        if (project.isDisposed || !Kargo.isKargoProject(project) || Kargo.executable() == null) return
        if (!running.compareAndSet(false, true)) {
            if (force) againForced.set(true)
            again.set(true)
            return
        }
        ProgressManager.getInstance().run(object : Task.Backgroundable(project, "Importing Kargo project", true) {
            override fun run(indicator: ProgressIndicator) {
                try {
                    import(indicator, force)
                } finally {
                    running.set(false)
                    if (again.getAndSet(false)) schedule(force = againForced.getAndSet(false))
                }
            }
        })
    }

    private fun import(indicator: ProgressIndicator, force: Boolean) {
        val properties = PropertiesComponent.getInstance(project)
        val known = properties.getValue(FINGERPRINT_KEY)
        if (!force && known != null && hasModules()) {
            val current = kargo(indicator, listOf("metadata", "--fingerprint"))?.trim()
            if (current == known) return
        }
        indicator.text = "Resolving the dependencies"
        val json = kargo(indicator, listOf("metadata"), report = true) ?: return
        val model = try {
            KargoProject.parse(json)
        } catch (e: Exception) {
            LOG.warn("Can't read kargo metadata", e)
            KargoNotifications.error(project, "Couldn't import the Kargo project", "Unexpected output from kargo metadata: ${e.message}")
            return
        }
        indicator.text = "Updating the project model"
        WriteAction.runAndWait<RuntimeException> { if (!project.isDisposed) apply(project, model) }
        model.fingerprint?.let { properties.setValue(FINGERPRINT_KEY, it) }
    }

    private fun hasModules() = ModuleManager.getInstance(project).modules.any { it.getOptionValue(MANAGED_OPTION) != null }

    /** `kargo <args>` in the project root: its output, or null when it failed (reported when `report`). */
    private fun kargo(indicator: ProgressIndicator, args: List<String>, report: Boolean = false): String? {
        val output = CapturingProcessHandler(Kargo.commandLine(project, args)).runProcessWithProgressIndicator(indicator, 10 * 60 * 1000)
        if (output.exitCode == 0) return output.stdout
        if (report && !output.isCancelled) {
            LOG.warn("kargo ${args.joinToString(" ")} failed: ${output.stderr}")
            val reason = output.stderr.lines().lastOrNull { it.isNotBlank() }?.trim().orEmpty()
            KargoNotifications.error(project, "Couldn't import the Kargo project", reason.ifEmpty { "kargo metadata exited with ${output.exitCode}" })
        }
        return null
    }

    override fun dispose() {}

    companion object {
        private val LOG = logger<KargoProjectImport>()
        private const val FINGERPRINT_KEY = "kargo.fingerprint"
        private val AFFECTS_PROJECT = setOf("fetch", "update", "add", "remove", "target", "init", "lock")

        /** A module option marking the modules Kargo manages, so the others are left alone. */
        const val MANAGED_OPTION = "kargo.managed"

        /** `ModuleTypeId.JAVA_MODULE`. */
        private const val JAVA_MODULE = "JAVA_MODULE"

        fun getInstance(project: Project): KargoProjectImport = project.service()

        /** Makes the project's modules match `kargo`. Needs a write action. */
        fun apply(project: Project, kargo: KargoProject) {
            val base = project.basePath ?: kargo.modules.firstOrNull()?.root ?: return
            val jdk = kargo.jdkHome?.let { findOrCreateJdk(it) }

            val moduleModel = ModuleManager.getInstance(project).getModifiableModel()
            val modules = HashMap<String, Module>()
            for (spec in kargo.modules) {
                modules[spec.name] = moduleModel.findModuleByName(spec.name)
                    ?: moduleModel.newModule("$base/.idea/${spec.name}.iml", JAVA_MODULE)
            }
            for (module in moduleModel.modules) {
                if (module.getOptionValue(MANAGED_OPTION) != null && module !in modules.values) moduleModel.disposeModule(module)
            }
            moduleModel.commit()

            for (spec in kargo.modules) {
                val module = modules.getValue(spec.name)
                module.setOption(MANAGED_OPTION, spec.target)
                val model = ModuleRootManager.getInstance(module).modifiableModel
                try {
                    configure(model, spec, modules, jdk)
                    model.commit()
                } catch (e: Throwable) {
                    model.dispose()
                    throw e
                }
            }
        }

        private fun configure(model: ModifiableRootModel, spec: KargoModule, modules: Map<String, Module>, jdk: Sdk?) {
            model.contentEntries.forEach { model.removeContentEntry(it) }
            model.orderEntries.filter { it !is ModuleSourceOrderEntry }.forEach { model.removeOrderEntry(it) }
            if (jdk != null) model.sdk = jdk else model.inheritSdk()

            val content = model.addContentEntry(url(spec.root))
            spec.sourceRoots.forEach { content.addSourceFolder(url(it), JavaSourceRootType.SOURCE) }
            spec.testRoots.forEach { content.addSourceFolder(url(it), JavaSourceRootType.TEST_SOURCE) }
            spec.resourceRoots.forEach { content.addSourceFolder(url(it), JavaResourceRootType.RESOURCE) }
            spec.testResourceRoots.forEach { content.addSourceFolder(url(it), JavaResourceRootType.TEST_RESOURCE) }
            spec.excluded.filter { FileUtil.isAncestor(spec.root, it, true) }.forEach { content.addExcludeFolder(url(it)) }

            for ((name, test) in spec.moduleDependencies) {
                val dependency = modules[name]?.takeIf { it != model.module } ?: continue
                model.addModuleOrderEntry(dependency).scope = if (test) DependencyScope.TEST else DependencyScope.COMPILE
            }
            for (lib in spec.libraries) {
                val library = model.moduleLibraryTable.createLibrary(lib.name)
                val libraryModel = library.modifiableModel
                libraryModel.addRoot(VfsUtil.getUrlForLibraryRoot(File(lib.classes)), OrderRootType.CLASSES)
                lib.sources?.let { libraryModel.addRoot(VfsUtil.getUrlForLibraryRoot(File(it)), OrderRootType.SOURCES) }
                libraryModel.commit()
                if (lib.test) model.findLibraryOrderEntry(library)?.scope = DependencyScope.TEST
            }
        }

        private fun url(path: String) = VfsUtilCore.pathToUrl(FileUtil.toSystemIndependentName(path))

        /** The IDE's JDK at `home`, registering it if it isn't known yet. */
        private fun findOrCreateJdk(home: String): Sdk? {
            val table = ProjectJdkTable.getInstance()
            val wanted = FileUtil.toSystemIndependentName(home)
            table.allJdks.firstOrNull { it.homePath?.let(FileUtil::toSystemIndependentName) == wanted }?.let { return it }
            val java = JavaSdk.getInstance()
            if (!java.isValidSdkHome(home)) return null
            val sdk = java.createJdk(java.suggestSdkName(null, home), home, false)
            table.addJdk(sdk)
            return sdk
        }
    }
}

/** Starts [KargoProjectImport]: it imports a Kargo project now, and one that gains a `kargo.toml` (`kargo init`) later. */
class KargoImportStartup : ProjectActivity {
    override suspend fun execute(project: Project) {
        KargoProjectImport.getInstance(project).start()
    }
}
