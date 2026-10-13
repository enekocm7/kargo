// The editor of a Kargo run configuration.

package io.github.enekocm7.kargo.run

import com.intellij.execution.configuration.EnvironmentVariablesComponent
import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.options.SettingsEditor
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.TextFieldWithBrowseButton
import com.intellij.ui.components.JBTextField
import com.intellij.ui.dsl.builder.AlignX
import com.intellij.ui.dsl.builder.panel
import javax.swing.JComponent

class KargoRunConfigurationEditor(project: Project) : SettingsEditor<KargoRunConfiguration>() {
    private val command = JBTextField()
    private val workingDirectory = TextFieldWithBrowseButton().apply {
        addBrowseFolderListener(project, FileChooserDescriptorFactory.createSingleFolderDescriptor().withTitle("Working Directory"))
    }
    private val env = EnvironmentVariablesComponent()

    override fun resetEditorFrom(configuration: KargoRunConfiguration) {
        command.text = configuration.command
        workingDirectory.text = configuration.workingDirectory.orEmpty()
        env.envData = configuration.env
    }

    override fun applyEditorTo(configuration: KargoRunConfiguration) {
        configuration.command = command.text.trim()
        configuration.workingDirectory = workingDirectory.text.trim().ifEmpty { null }
        configuration.env = env.envData
    }

    override fun createEditor(): JComponent = panel {
        row("Command:") {
            cell(command).align(AlignX.FILL)
                .comment("Arguments after kargo, such as <code>run -t jvm</code>, <code>test MathTest</code> or <code>task lint</code>")
        }
        row("Working directory:") {
            cell(workingDirectory).align(AlignX.FILL).comment("The package directory; empty: the project root")
        }
        row(env.label) {
            cell(env.component).align(AlignX.FILL)
        }
    }
}
