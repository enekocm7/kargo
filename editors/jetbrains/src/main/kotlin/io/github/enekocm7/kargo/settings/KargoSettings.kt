// The plugin's settings, shared by every project: where `kargo` is and how `kargo lsp` runs.

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

        /** Whether to run `kargo lsp` for Kotlin files and `kargo.toml`. */
        var languageServer by property(true)

        /** `kargo lsp --server`: `kotlin-lsp`, `kls` or `none`. */
        var server by string(DEFAULT_SERVER)

        /** `kargo lsp --klib-targets`. */
        var klibTargets by property(false)

        /** The `checkOnSave` initialization option. */
        var checkOnSave by property(true)

        /** The `checkTargets` initialization option, comma-separated; empty: every target. */
        var checkTargets by string()
    }

    companion object {
        const val DEFAULT_SERVER = "kotlin-lsp"
        val SERVERS = listOf(DEFAULT_SERVER, "kls", "none")

        fun getInstance(): KargoSettings = service()
    }
}
