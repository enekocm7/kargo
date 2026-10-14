// What the import remembers between IDE sessions, in .idea/kargo.xml: which modules it manages (so it can remove
// the ones Kargo no longer lists without touching the user's own), and the state of the last import.

package io.github.enekocm7.kargo.project

import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project

@Service(Service.Level.PROJECT)
@State(name = "KargoImport", storages = [Storage("kargo.xml")])
class KargoImportState : SimplePersistentStateComponent<KargoImportState.Options>(Options()) {
    class Options : BaseState() {
        /** The modules the import created or took over. */
        var modules by list<String>()

        /** `kargo metadata --fingerprint` of the last successful import: unchanged means nothing to do. */
        var fingerprint by string()

        /** The files whose change makes the import stale (`watch` of `kargo metadata`). */
        var watched by list<String>()

        /** The package directories, whose `src*` and `tests*` directories coming and going change the roots. */
        var packageRoots by list<String>()
    }

    companion object {
        fun getInstance(project: Project): KargoImportState = project.service()
    }
}
