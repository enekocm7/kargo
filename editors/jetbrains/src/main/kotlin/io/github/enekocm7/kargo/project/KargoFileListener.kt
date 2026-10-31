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
            } else if (isLayoutChange(event) && isUnderRoots(path, roots)) {
                // Any directory appearing or disappearing under a package (such as a `[layout]`
                // directory) may be a new source root, so the project is imported again.
                layout = true
            }
        }
        // A changed kargo.toml or kargo.lock is checked against the last import's fingerprint (our own `kargo metadata`
        // rewrites kargo.lock, which ends up here); a directory coming or going isn't part of the fingerprint.
        if (manifests || layout) KargoProjectSync.getInstance(project).requestSync(force = layout)
    }

    private fun isUnderRoots(path: String, roots: List<String>): Boolean {
        // The event itself is a directory, or a directory's contents changed (the path is a file).
        val dir = path.substringBeforeLast('/', "")
        return roots.any { root -> dir == root || dir.startsWith("$root/") || path == root }
    }

    private fun isLayoutChange(event: VFileEvent) =
        event is VFileCreateEvent || event is VFileDeleteEvent || event is VFileMoveEvent || (event is VFilePropertyChangeEvent && event.isRename)

    companion object {
        const val LOCKFILE = "kargo.lock"
    }
}
