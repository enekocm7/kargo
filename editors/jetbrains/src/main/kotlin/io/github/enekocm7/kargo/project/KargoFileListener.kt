// Imports the project again when what it was imported from changes: a kargo.toml or kargo.lock, or a source,
// test or resource directory of a package appearing or disappearing.

package io.github.enekocm7.kargo.project

import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.newvfs.BulkFileListener
import com.intellij.openapi.vfs.newvfs.events.VFileCreateEvent
import com.intellij.openapi.vfs.newvfs.events.VFileDeleteEvent
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import com.intellij.openapi.vfs.newvfs.events.VFileMoveEvent
import com.intellij.openapi.vfs.newvfs.events.VFilePropertyChangeEvent
import io.github.enekocm7.kargo.Kargo

class KargoFileListener(private val project: Project) : BulkFileListener {
    override fun after(events: List<VFileEvent>) {
        if (project.isDisposed || !Kargo.isKargoProject(project)) return
        val roots = KargoImportState.getInstance(project).state.packageRoots.map(::norm)
        var manifests = false
        var layout = false
        for (event in events) {
            val path = norm(event.path)
            val name = path.substringAfterLast('/')
            if (name == Kargo.MANIFEST || name == LOCKFILE) {
                manifests = true
            } else if (isLayoutChange(event) && LAYOUT_DIR.matches(name) && path.substringBeforeLast('/', "") in roots) {
                layout = true
            }
        }
        // A changed kargo.toml or kargo.lock is checked against the last import's fingerprint (our own `kargo metadata`
        // rewrites kargo.lock, which ends up here); a directory coming or going isn't part of the fingerprint.
        if (manifests || layout) KargoProjectSync.getInstance(project).requestSync(force = layout)
    }

    private fun isLayoutChange(event: VFileEvent) =
        event is VFileCreateEvent || event is VFileDeleteEvent || event is VFileMoveEvent || (event is VFilePropertyChangeEvent && event.isRename)

    companion object {
        const val LOCKFILE = "kargo.lock"

        /** The directories Kargo takes roots from: `src`, `src-jvm`, `tests`, `tests-native`, `resources`, `test-resources`. */
        val LAYOUT_DIR = Regex("(src|tests)(-[a-z]+)?|resources|test-resources")
    }
}
