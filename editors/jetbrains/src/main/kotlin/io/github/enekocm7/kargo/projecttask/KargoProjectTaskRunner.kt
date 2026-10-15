// Build | Build Project runs `kargo build` for the modules Kargo imported, instead of the IDE's own compiler.
// Its compiler messages go to the Build tool window as build messages, with links to the sources, so they
// also show in the Problems view.

package io.github.enekocm7.kargo.projecttask

import com.intellij.build.BuildViewManager
import com.intellij.build.DefaultBuildDescriptor
import com.intellij.build.FilePosition
import com.intellij.build.events.BuildEvent
import com.intellij.build.events.MessageEvent
import com.intellij.build.events.impl.FailureResultImpl
import com.intellij.build.events.impl.FileMessageEventImpl
import com.intellij.build.events.impl.FinishBuildEventImpl
import com.intellij.build.events.impl.MessageEventImpl
import com.intellij.build.events.impl.OutputBuildEventImpl
import com.intellij.build.events.impl.StartBuildEventImpl
import com.intellij.build.events.impl.SuccessResultImpl
import com.intellij.execution.ExecutionException
import com.intellij.execution.process.OSProcessHandler
import com.intellij.execution.process.ProcessAdapter
import com.intellij.execution.process.ProcessEvent
import com.intellij.execution.process.ProcessOutputTypes
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Key
import com.intellij.task.ModuleBuildTask
import com.intellij.task.ProjectTask
import com.intellij.task.ProjectTaskContext
import com.intellij.task.ProjectTaskRunner
import io.github.enekocm7.kargo.Kargo
import io.github.enekocm7.kargo.KargoNotifications
import io.github.enekocm7.kargo.project.KargoImportState
import io.github.enekocm7.kargo.project.KargoProjectSync
import org.jetbrains.concurrency.AsyncPromise
import org.jetbrains.concurrency.Promise
import java.io.File

/** Runs `kargo build` for the Kargo modules of the project, once for all of them. */
class KargoProjectTaskRunner : ProjectTaskRunner() {
    override fun canRun(projectTask: ProjectTask): Boolean {
        val module = (projectTask as? ModuleBuildTask)?.module ?: return false
        return KargoImportState.getInstance(module.project).state.modules.contains(module.name)
    }

    override fun run(project: Project, context: ProjectTaskContext, vararg tasks: ProjectTask): Promise<Result> {
        val promise = AsyncPromise<Result>()
        val dir = Kargo.rootManifest(project)?.parent?.path ?: project.basePath
        val buildId = Any()
        val buildView = BuildViewManager.getInstance(project)
        buildView.onEvent(buildId, StartBuildEventImpl(DefaultBuildDescriptor(buildId, "Kargo build", dir.orEmpty(), System.currentTimeMillis()), "kargo build"))

        val reader = KargoDiagnosticReader()
        var errors = 0
        var warnings = 0
        fun report(diagnostic: KargoDiagnostic) {
            when (diagnostic.severity) {
                "error", "exception" -> errors++
                "warning" -> warnings++
            }
            buildView.onEvent(buildId, diagnosticEvent(buildId, diagnostic))
        }

        val handler = try {
            OSProcessHandler(Kargo.commandLine(project, listOf("build", "--message-format", "json"), dir))
        } catch (e: ExecutionException) {
            finish(buildView, buildId, promise, errors, warnings, success = false)
            KargoNotifications.error(project, "Couldn't start kargo build", e.message ?: e.toString())
            return promise
        }
        handler.addProcessListener(object : ProcessAdapter() {
            override fun onTextAvailable(event: ProcessEvent, outputType: Key<*>) {
                if (outputType == ProcessOutputTypes.STDOUT) {
                    reader.accept(event.text).forEach(::report)
                } else {
                    buildView.onEvent(buildId, OutputBuildEventImpl(buildId, event.text, false))
                }
            }

            override fun processTerminated(event: ProcessEvent) {
                reader.finish().forEach(::report)
                finish(buildView, buildId, promise, errors, warnings, success = event.exitCode == 0)
                // A build can fetch dependencies and change kargo.lock; the project model follows.
                KargoProjectSync.getInstance(project).refreshAndSync()
            }
        })
        handler.startNotify()
        return promise
    }

    private fun finish(buildView: BuildViewManager, buildId: Any, promise: AsyncPromise<Result>, errors: Int, warnings: Int, success: Boolean) {
        val result = if (success) SuccessResultImpl() else FailureResultImpl()
        buildView.onEvent(buildId, FinishBuildEventImpl(buildId, null, System.currentTimeMillis(), if (success) "finished" else "failed", result))
        promise.setResult(BuildResult(errors, warnings, aborted = false))
    }

    /** The build message for a diagnostic: at its location when the compiler gave one, else on the build itself. */
    private fun diagnosticEvent(buildId: Any, diagnostic: KargoDiagnostic): BuildEvent {
        val kind = when (diagnostic.severity) {
            "error", "exception" -> MessageEvent.Kind.ERROR
            "warning" -> MessageEvent.Kind.WARNING
            else -> MessageEvent.Kind.INFO
        }
        val file = diagnostic.file?.let(::File)?.takeIf { it.isFile }
        val line = diagnostic.line
        return if (file != null && line != null) {
            val position = FilePosition(file, (line - 1).coerceAtLeast(0), ((diagnostic.column ?: 1) - 1).coerceAtLeast(0))
            FileMessageEventImpl(buildId, kind, GROUP, diagnostic.message, diagnostic.rendered, position)
        } else {
            MessageEventImpl(buildId, kind, GROUP, diagnostic.message, diagnostic.rendered)
        }
    }

    private class BuildResult(private val errors: Int, private val warnings: Int, private val aborted: Boolean) : Result {
        override fun isAborted(): Boolean = aborted

        override fun getErrors(): Int = errors

        override fun getWarnings(): Int = warnings
    }

    companion object {
        /** The group of the build messages, shown in the Build tool window and the Problems view. */
        private const val GROUP = "Kargo"
    }
}
