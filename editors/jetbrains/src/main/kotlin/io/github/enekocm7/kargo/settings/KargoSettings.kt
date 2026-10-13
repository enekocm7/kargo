// The plugin's settings, shared by every project: where `kargo` is and whether `kargo lsp` runs.

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
    }

    companion object {
        fun getInstance(): KargoSettings = service()
    }
}
