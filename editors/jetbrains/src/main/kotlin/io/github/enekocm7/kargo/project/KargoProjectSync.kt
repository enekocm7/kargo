// Keeps the IDE's project model in step with Kargo: runs `kargo metadata` in the background when a project
// opens and when kargo.toml or kargo.lock change, and applies the result with KargoProjectImporter.

package io.github.enekocm7.kargo.project

import com.intellij.execution.process.CapturingProcessHandler
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.diagnostic.logger
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.progress.ProcessCanceledException
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.util.Alarm
import io.github.enekocm7.kargo.Kargo
import io.github.enekocm7.kargo.KargoNotifications
import io.github.enekocm7.kargo.settings.KargoSettings
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean

@Service(Service.Level.PROJECT)
class KargoProjectSync(private val project: Project) : Disposable {
    private val log = logger<KargoProjectSync>()
    private val alarm = Alarm(Alarm.ThreadToUse.POOLED_THREAD, this)
    private val running = AtomicBoolean(false)

    @Volatile
    private var pending = false

    @Volatile
    private var forcePending = false

    /**
     * Imports the project soon, after a short pause that merges bursts of changes. Unless `force` is set, nothing happens
     * when `kargo metadata --fingerprint` shows that kargo.toml and kargo.lock are as they were at the last import.
     */
    fun requestSync(force: Boolean = false, delayMs: Int = 400) {
        if (!KargoSettings.getInstance().state.importProject || !Kargo.isKargoProject(project) || project.isDisposed) return
        if (force) forcePending = true
        alarm.cancelAllRequests()
        alarm.addRequest(::start, delayMs)
    }

    /**
     * Looks at the files on disk again, then imports: for after a `kargo` command may have rewritten them. `force`
     * imports even when kargo.toml and kargo.lock are unchanged, as after `kargo fetch --sources`.
     */
    fun refreshAndSync(force: Boolean = false) {
        val watched = KargoImportState.getInstance(project).state.watched.map(::File)
        if (watched.isNotEmpty()) LocalFileSystem.getInstance().refreshIoFiles(watched, true, false) { requestSync(force) } else requestSync(force)
    }

    private fun start() {
        if (project.isDisposed) return
        if (!running.compareAndSet(false, true)) {
            pending = true
            return
        }
        val force = forcePending
        forcePending = false
        ProgressManager.getInstance().run(object : Task.Backgroundable(project, "Importing Kargo project", true) {
            override fun run(indicator: ProgressIndicator) {
                try {
                    importOnce(force, indicator)
                } catch (e: ProcessCanceledException) {
                    throw e
                } catch (e: Throwable) {
                    log.warn("importing the Kargo project failed", e)
                    KargoNotifications.error(project, "Couldn't import the Kargo project", e.message ?: e.toString())
                } finally {
                    running.set(false)
                    if (pending) {
                        pending = false
                        requestSync(forcePending, 100)
                    }
                }
            }
        })
    }

    /** One import, run to the end on the calling thread. Returns whether the project model was updated. */
    internal fun importOnce(force: Boolean, indicator: ProgressIndicator): Boolean {
        val state = KargoImportState.getInstance(project).state
        if (Kargo.executable() == null) return false
        if (!force) {
            val fingerprint = run(listOf("metadata", "--fingerprint"), indicator)?.takeIf { it.exitCode == 0 }?.stdout?.trim()
            if (fingerprint != null && fingerprint == state.fingerprint && modulesExist(state.modules)) return false
        }

        indicator.text = "Resolving dependencies with kargo metadata"
        var resolved = true
        var failure: String? = null
        var output = run(listOf("metadata", "-q"), indicator)
        if (output?.isCancelled == true) return false
        if (output == null || output.exitCode != 0) {
            failure = output?.stderr?.trim().takeUnless { it.isNullOrEmpty() } ?: "kargo metadata failed"
            // Without dependencies the source roots are still useful: keep the modules and say what went wrong.
            resolved = false
            output = run(listOf("metadata", "-q", "--no-deps"), indicator)
        }
        if (output?.isCancelled == true) return false
        if (output == null || output.exitCode != 0) {
            val reason = output?.stderr?.trim().takeUnless { it.isNullOrEmpty() } ?: failure ?: "kargo metadata failed"
            log.warn("kargo metadata failed: $reason")
            KargoNotifications.error(project, "Couldn't import the Kargo project", reason)
            return false
        }

        val metadata = KargoMetadata.parse(output.stdout)
        if (metadata.version > SUPPORTED_METADATA_VERSION) {
            log.warn("kargo metadata has format version ${metadata.version}, newer than the ${SUPPORTED_METADATA_VERSION} this plugin knows")
        }
        indicator.text = "Updating the project model"
        val plan = KargoImportPlanner.plan(metadata)
        val result = WriteAction.computeAndWait<KargoProjectImporter.Result, RuntimeException> { KargoProjectImporter.apply(project, plan) }
        state.fingerprint = if (resolved) metadata.fingerprint else null
        state.watched = metadata.watch.toMutableList()
        state.packageRoots = metadata.packages.map { it.root }.toMutableList()
        for (problem in result.failures) log.warn("import: $problem")
        if (failure != null) {
            KargoNotifications.warning(project, "Kargo dependencies are not resolved", "Source roots were imported, but libraries could not be: $failure")
        } else if (result.failures.isNotEmpty()) {
            KargoNotifications.warning(project, "Kargo import is incomplete", result.failures.joinToString("\n"))
        }
        return true
    }

    private fun modulesExist(names: List<String>): Boolean {
        if (names.isEmpty()) return false
        val manager = ModuleManager.getInstance(project)
        return names.all { manager.findModuleByName(it) != null }
    }

    private fun run(args: List<String>, indicator: ProgressIndicator): com.intellij.execution.process.ProcessOutput? {
        val handler = try {
            CapturingProcessHandler(Kargo.commandLine(project, args))
        } catch (e: Exception) {
            log.warn("couldn't start kargo", e)
            return null
        }
        return handler.runProcessWithProgressIndicator(indicator, TIMEOUT_MS)
    }

    override fun dispose() {}

    companion object {
        /** Resolving dependencies can download them; allow it a long time. */
        private const val TIMEOUT_MS = 15 * 60 * 1000

        fun getInstance(project: Project): KargoProjectSync = project.service()
    }
}
