// Tools | Kargo: Kargo's commands from the IDE. Commands run as temporary Kargo run
// configurations, so their output lands in the Run tool window with links to the sources.

package io.github.enekocm7.kargo.actions

import com.google.gson.JsonObject
import com.intellij.execution.executors.DefaultRunExecutor
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.CommonDataKeys
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.guessProjectDir
import com.intellij.openapi.ui.Messages
import com.intellij.openapi.ui.popup.JBPopupFactory
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.util.execution.ParametersListUtil
import io.github.enekocm7.kargo.Kargo
import io.github.enekocm7.kargo.KargoNotifications
import io.github.enekocm7.kargo.lsp.KargoLsp
import io.github.enekocm7.kargo.run.runKargo

/** The package of the file in the editor or selected in the project view, else the project root. */
private fun packageDir(e: AnActionEvent): VirtualFile? =
    e.getData(CommonDataKeys.VIRTUAL_FILE)?.let { Kargo.packageDir(it) } ?: e.project?.let { Kargo.rootManifest(it)?.parent }

abstract class KargoProjectAction : DumbAwareAction() {
    override fun getActionUpdateThread() = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        e.presentation.isEnabledAndVisible = e.project?.let { Kargo.isKargoProject(it) } == true
    }
}

/** Runs `kargo <args>` in the current package. */
open class KargoCommandAction(private val args: List<String>) : KargoProjectAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        runKargo(project, "kargo ${args.joinToString(" ")}", args, packageDir(e)?.path)
    }
}

class KargoBuildAction : KargoCommandAction(listOf("build"))
class KargoCheckAction : KargoCommandAction(listOf("check"))
class KargoTestAction : KargoCommandAction(listOf("test"))
class KargoCleanAction : KargoCommandAction(listOf("clean"))
class KargoFetchAction : KargoCommandAction(listOf("fetch", "--sources"))
class KargoUpdateAction : KargoCommandAction(listOf("update"))

/** Asks for any `kargo` command line and runs it. */
class KargoRunCommandAction : KargoProjectAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val command = Messages.showInputDialog(project, "kargo", "Run Kargo Command", null, "build", null)?.trim() ?: return
        val args = ParametersListUtil.parse(command).dropWhile { it == "kargo" }
        if (args.isNotEmpty()) runKargo(project, "kargo ${args.joinToString(" ")}", args, packageDir(e)?.path)
    }
}

/** `kargo add`, through `kargo lsp`'s `kargo/addDependency` so the version is looked up the same way. */
class KargoAddDependencyAction : KargoProjectAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val manifest = packageDir(e)?.findChild(Kargo.MANIFEST) ?: return
        val coords = Messages.showInputDialog(
            project, "Coordinates (group:artifact or group:artifact:version):", "Add Dependency", null,
        )?.trim()?.takeIf { it.isNotEmpty() } ?: return
        val dev = Messages.showYesNoDialog(project, "Add $coords to [dependencies] or [dev-dependencies]?", "Add Dependency", "dependencies", "dev-dependencies", null) == Messages.NO
        val arguments = JsonObject().apply {
            addProperty("coords", coords)
            addProperty("section", if (dev) "dev-dependencies" else "dependencies")
        }
        KargoLsp.executeCommand(project, "kargo/addDependency", manifest, arguments).whenComplete { _, error ->
            if (error != null) KargoNotifications.error(project, "Couldn't add $coords", error.cause?.message ?: error.message ?: "")
        }
    }
}

/** `kargo remove <name>`. */
class KargoRemoveDependencyAction : KargoProjectAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val name = Messages.showInputDialog(project, "Dependency to remove (group:artifact or its name in kargo.toml):", "Remove Dependency", null)
            ?.trim()?.takeIf { it.isNotEmpty() } ?: return
        runKargo(project, "kargo remove $name", listOf("remove", name), packageDir(e)?.path)
    }
}

/** `kargo target add`, through `kargo lsp`'s `kargo/addTarget`. */
class KargoAddTargetAction : KargoProjectAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val manifest = packageDir(e)?.findChild(Kargo.MANIFEST) ?: return
        JBPopupFactory.getInstance().createPopupChooserBuilder(listOf("jvm", "wasm", "native", "android"))
            .setTitle("Add Target")
            .setItemChosenCallback { target ->
                val arguments = JsonObject().apply { addProperty("target", target) }
                KargoLsp.executeCommand(project, "kargo/addTarget", manifest, arguments).whenComplete { _, error ->
                    if (error != null) KargoNotifications.error(project, "Couldn't add the $target target", error.cause?.message ?: error.message ?: "")
                }
            }
            .createPopup()
            .showCenteredInCurrentWindow(project)
    }
}

/** `kargo task <name>`, choosing among the tasks `kargo lsp` lists. */
class KargoRunTaskAction : KargoProjectAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val file = e.getData(CommonDataKeys.VIRTUAL_FILE)
        KargoLsp.runnables(project, file).whenComplete { runnables, error ->
            ApplicationManager.getApplication().invokeLater {
                val tasks = runnables.orEmpty().filter { it.kind == "task" }
                if (tasks.isEmpty()) {
                    val reason = error?.message ?: "No tasks in kargo.toml, or kargo lsp isn't running yet"
                    KargoNotifications.info(project, "No Kargo tasks", reason)
                    return@invokeLater
                }
                JBPopupFactory.getInstance().createPopupChooserBuilder(tasks)
                    .setTitle("Run Kargo Task")
                    .setItemChosenCallback { runKargo(project, it.label, it.args, it.cwd, DefaultRunExecutor.getRunExecutorInstance()) }
                    .createPopup()
                    .showCenteredInCurrentWindow(project)
            }
        }
    }
}

/** Restarts `kargo lsp`, which imports the project again. */
class KargoReloadAction : KargoProjectAction() {
    override fun actionPerformed(e: AnActionEvent) {
        KargoLsp.restart(e.project ?: return)
    }
}

/** `kargo init` in the root of a project that isn't a Kargo project yet. */
class KargoInitAction : DumbAwareAction() {
    override fun getActionUpdateThread() = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        val project = e.project
        e.presentation.isEnabledAndVisible = project != null && project.guessProjectDir() != null && !Kargo.isKargoProject(project)
    }

    override fun actionPerformed(e: AnActionEvent) {
        val project: Project = e.project ?: return
        val root = project.guessProjectDir() ?: return
        runKargo(project, "kargo init", listOf("init"), root.path)
    }
}
