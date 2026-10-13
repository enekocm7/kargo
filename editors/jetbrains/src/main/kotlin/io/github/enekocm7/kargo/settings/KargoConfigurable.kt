// Settings | Languages & Frameworks | Kargo.

package io.github.enekocm7.kargo.settings

import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.options.BoundConfigurable
import com.intellij.openapi.project.ProjectManager
import com.intellij.ui.dsl.builder.bindSelected
import com.intellij.ui.dsl.builder.bindText
import com.intellij.ui.dsl.builder.columns
import com.intellij.ui.dsl.builder.COLUMNS_LARGE
import com.intellij.ui.dsl.builder.panel
import io.github.enekocm7.kargo.Kargo
import io.github.enekocm7.kargo.lsp.KargoLsp

class KargoConfigurable : BoundConfigurable("Kargo") {
    private val options get() = KargoSettings.getInstance().state

    override fun createPanel() = panel {
        group("Kargo") {
            row("Kargo executable:") {
                textFieldWithBrowseButton(FileChooserDescriptorFactory.createSingleFileNoJarsDescriptor().withTitle("Kargo Executable"))
                    .bindText({ options.kargoPath ?: "" }, { options.kargoPath = it.trim().ifEmpty { null } })
                    .columns(COLUMNS_LARGE)
                    .comment("Leave empty to use the kargo on PATH. Found: ${Kargo.executable()?.path ?: "none"}")
            }
        }
        group("Language Server (kargo lsp)") {
            row {
                checkBox("Run kargo lsp for kargo.toml")
                    .bindSelected({ options.languageServer }, { options.languageServer = it })
                    .comment("Completion, documentation, validation and quick fixes in kargo.toml. Kotlin files use the IDE's own support")
            }
        }
    }

    override fun apply() {
        super.apply()
        for (project in ProjectManager.getInstance().openProjects) {
            if (Kargo.isKargoProject(project)) KargoLsp.restart(project)
        }
    }
}
