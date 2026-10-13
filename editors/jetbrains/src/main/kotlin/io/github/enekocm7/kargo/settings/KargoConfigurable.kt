// Settings | Languages & Frameworks | Kargo.

package io.github.enekocm7.kargo.settings

import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.options.BoundConfigurable
import com.intellij.openapi.project.ProjectManager
import com.intellij.ui.dsl.builder.bindItem
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
                checkBox("Run kargo lsp for Kotlin files and kargo.toml")
                    .bindSelected({ options.languageServer }, { options.languageServer = it })
            }
            row("Kotlin server:") {
                comboBox(KargoSettings.SERVERS)
                    .bindItem({ options.server ?: KargoSettings.DEFAULT_SERVER }, { options.server = it ?: KargoSettings.DEFAULT_SERVER })
                    .comment("kotlin-lsp: JetBrains' Kotlin LSP; kls: kotlin-language-server; none: only kargo.toml support and runnables")
            }
            row {
                checkBox("Describe Wasm and Native sources to the Kotlin LSP (--klib-targets)")
                    .bindSelected({ options.klibTargets }, { options.klibTargets = it })
            }
            row {
                checkBox("Check on save: run kargo check when a Kotlin file is saved")
                    .bindSelected({ options.checkOnSave }, { options.checkOnSave = it })
            }
            row("Targets to check:") {
                textField()
                    .bindText({ options.checkTargets ?: "" }, { options.checkTargets = it.trim().ifEmpty { null } })
                    .comment("Comma-separated, for example wasm,native. Empty: every enabled target")
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
