// File | New | Project | Kargo: the options of `kargo init` as a form, run in the new project's
// directory.

package io.github.enekocm7.kargo.wizard

import com.intellij.execution.process.CapturingProcessHandler
import com.intellij.execution.process.ProcessOutput
import com.intellij.ide.wizard.AbstractNewProjectWizardStep
import com.intellij.ide.wizard.GeneratorNewProjectWizard
import com.intellij.ide.wizard.NewProjectWizardBaseStep
import com.intellij.ide.wizard.NewProjectWizardChainStep.Companion.nextStep
import com.intellij.ide.wizard.NewProjectWizardStep
import com.intellij.ide.wizard.RootNewProjectWizardStep
import com.intellij.ide.util.projectWizard.WizardContext
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.StartupManager
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.VfsUtil
import com.intellij.ui.dsl.builder.Panel
import com.intellij.ui.dsl.builder.bindSelected
import com.intellij.ui.dsl.builder.bindText
import io.github.enekocm7.kargo.Kargo
import io.github.enekocm7.kargo.KargoIcons
import java.io.File
import javax.swing.Icon

class KargoNewProjectWizard : GeneratorNewProjectWizard {
    override val id: String = "kargo"
    override val name: String = "Kargo"
    override val icon: Icon = KargoIcons.KARGO

    override fun createStep(context: WizardContext): NewProjectWizardStep =
        RootNewProjectWizardStep(context).nextStep(::NewProjectWizardBaseStep).nextStep(::KargoProjectStep)
}

class KargoProjectStep(private val base: NewProjectWizardBaseStep) : AbstractNewProjectWizardStep(base) {
    private val library = propertyGraph.property(false)
    private val targets = TARGETS.associateWith { propertyGraph.property(it == "jvm") }
    private val frameworks = FRAMEWORKS.associateWith { propertyGraph.property(false) }
    private val group = propertyGraph.property("com.example")
    private val kotlin = propertyGraph.property("")
    private val git = propertyGraph.property(true)

    override fun setupUI(builder: Panel) {
        with(builder) {
            row {
                checkBox("Library").bindSelected(library).comment("A library instead of an application (--lib)")
            }
            row("Targets:") {
                for ((target, selected) in targets) checkBox(target).bindSelected(selected)
            }
            row("Frameworks:") {
                for ((framework, selected) in frameworks) checkBox(framework).bindSelected(selected)
            }
            row("Group:") {
                textField().bindText(group).comment("Maven group and base package")
            }
            row("Kotlin version:") {
                textField().bindText(kotlin).comment("Empty: the version Kargo pins")
            }
            row {
                checkBox("Create Git repository").bindSelected(git)
            }
            if (Kargo.executable() == null) {
                row {
                    comment("Kargo was not found on PATH. <a href=\"${Kargo.INSTALL_URL}\">Install Kargo</a> or set its path in Settings | Languages & Frameworks | Kargo.")
                }
            }
        }
    }

    /** The arguments of `kargo init` for the options chosen. */
    fun initArgs(dir: String): List<String> {
        val args = mutableListOf("init", dir)
        if (library.get()) args += "--lib"
        val chosenTargets = targets.filterValues { it.get() }.keys
        if (chosenTargets.isNotEmpty() && chosenTargets != setOf("jvm")) args += listOf("--target", chosenTargets.joinToString(","))
        val chosenFrameworks = frameworks.filterValues { it.get() }.keys
        if (chosenFrameworks.isNotEmpty()) args += listOf("--with", chosenFrameworks.joinToString(","))
        group.get().trim().takeIf { it.isNotEmpty() && it != "com.example" }?.let { args += listOf("--group", it) }
        kotlin.get().trim().takeIf { it.isNotEmpty() }?.let { args += listOf("--kotlin", it) }
        if (!git.get()) args += "--no-vcs"
        return args
    }

    override fun setupProject(project: Project) {
        val dir = File(base.path, base.name).path
        val commandLine = Kargo.commandLine(project, initArgs(dir), base.path)
        val output = ProgressManager.getInstance().runProcessWithProgressSynchronously<ProcessOutput, Exception>(
            { CapturingProcessHandler(commandLine).runProcess(120_000) }, "Creating Kargo project", false, project,
        )
        if (output.exitCode != 0 || output.isTimeout) {
            NotificationGroupManager.getInstance().getNotificationGroup("Kargo")
                .createNotification("Kargo couldn't create the project", output.stderr.ifBlank { "kargo init failed" }, NotificationType.ERROR)
                .notify(project)
            return
        }
        val root = LocalFileSystem.getInstance().refreshAndFindFileByPath(dir) ?: return
        VfsUtil.markDirtyAndRefresh(false, true, true, root)
        val main = root.findFileByRelativePath("src/Main.kt") ?: root.findChild(Kargo.MANIFEST) ?: return
        // Opening a file starts `kargo lsp`; the project's frame isn't open yet.
        StartupManager.getInstance(project).runAfterOpened {
            ApplicationManager.getApplication().invokeLater({ FileEditorManager.getInstance(project).openFile(main, true) }, project.disposed)
        }
    }

    companion object {
        val TARGETS = listOf("jvm", "wasm", "native", "android")
        val FRAMEWORKS = listOf("serialization", "coroutines", "compose", "ktor", "lombok", "spring-boot")
    }
}
