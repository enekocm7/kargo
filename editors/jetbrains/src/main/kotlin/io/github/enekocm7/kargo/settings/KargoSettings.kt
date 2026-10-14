// The plugin's settings, shared by every project: where `kargo` is, whether `kargo lsp` runs and whether projects are imported.

package io.github.enekocm7.kargo.settings

import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.service

@Service(Service.Level.APP)
@State(name = "KargoSettings", storages = [Storage("kargo.xml")])
class KargoSettings : SimplePersistentStateComponent<KargoSettings.Options>(Options()) {
    class Options : BaseState() {
        /** The `kargo` executable; empty: look it up on `PATH`. */
        var kargoPath by string()

        /** Whether to run `kargo lsp` for `kargo.toml`. */
        var languageServer by property(true)

        /** Whether to describe the project to the IDE (modules, source roots, libraries) from `kargo metadata`. */
        var importProject by property(true)
    }

    companion object {
        fun getInstance(): KargoSettings = service()
    }
}
