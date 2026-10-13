// Talking to the running `kargo lsp`: runnables, commands and restarts.

package io.github.enekocm7.kargo.lsp

import com.google.gson.JsonObject
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VfsUtil
import com.intellij.openapi.vfs.VirtualFile
import com.redhat.devtools.lsp4ij.LSPIJUtils
import com.redhat.devtools.lsp4ij.LanguageServerManager
import org.eclipse.lsp4j.ExecuteCommandParams
import org.eclipse.lsp4j.TextDocumentIdentifier
import java.util.concurrent.CompletableFuture

object KargoLsp {
    /** The id of the server in `plugin.xml`. */
    const val SERVER_ID = "kargo"

    private fun server(project: Project): CompletableFuture<KargoLanguageServer?> =
        LanguageServerManager.getInstance(project).getLanguageServer(SERVER_ID).thenApply { it?.server as? KargoLanguageServer }

    /** The runnables of the package containing `file`, or of every member without one. Empty when the server isn't running. */
    fun runnables(project: Project, file: VirtualFile?): CompletableFuture<List<KargoRunnable>> = server(project).thenCompose { server ->
        if (server == null) {
            CompletableFuture.completedFuture(emptyList())
        } else {
            val document = file?.let { TextDocumentIdentifier(LSPIJUtils.toUriAsString(it)) }
            server.runnables(RunnablesParams(document)).thenApply { it.orEmpty() }
        }
    }

    /**
     * Runs one of Kargo's `workspace/executeCommand` commands (`kargo/addDependency`, `kargo/addTarget`)
     * on `manifest`, then refreshes it, since Kargo writes the file itself when it isn't open.
     */
    fun executeCommand(project: Project, command: String, manifest: VirtualFile, arguments: JsonObject): CompletableFuture<Any?> =
        server(project).thenCompose { server ->
            if (server == null) {
                CompletableFuture.failedFuture(IllegalStateException("kargo lsp is not running; open a Kotlin file or kargo.toml to start it"))
            } else {
                arguments.addProperty("uri", LSPIJUtils.toUriAsString(manifest))
                server.workspaceService.executeCommand(ExecuteCommandParams(command, listOf(arguments)))
            }
        }.whenComplete { _, _ -> VfsUtil.markDirtyAndRefresh(true, true, true, manifest.parent) }

    /** Stops `kargo lsp` and starts it again, with the current settings. */
    fun restart(project: Project) {
        val manager = LanguageServerManager.getInstance(project)
        manager.stop(SERVER_ID, LanguageServerManager.StopOptions().setWillDisable(false))
        manager.start(SERVER_ID)
    }
}
