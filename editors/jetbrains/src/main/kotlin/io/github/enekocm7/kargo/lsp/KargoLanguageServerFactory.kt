// `kargo lsp` as an LSP4IJ language server: how it starts, the options it gets, and the requests
// Kargo adds to LSP.

package io.github.enekocm7.kargo.lsp

import com.google.gson.JsonArray
import com.google.gson.JsonObject
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile
import com.redhat.devtools.lsp4ij.LanguageServerEnablementSupport
import com.redhat.devtools.lsp4ij.LanguageServerFactory
import com.redhat.devtools.lsp4ij.server.OSProcessStreamConnectionProvider
import com.redhat.devtools.lsp4ij.server.StreamConnectionProvider
import io.github.enekocm7.kargo.Kargo
import io.github.enekocm7.kargo.settings.KargoSettings
import org.eclipse.lsp4j.TextDocumentIdentifier
import org.eclipse.lsp4j.jsonrpc.services.JsonRequest
import org.eclipse.lsp4j.services.LanguageServer
import java.util.concurrent.CompletableFuture

class KargoLanguageServerFactory : LanguageServerFactory, LanguageServerEnablementSupport {
    override fun createConnectionProvider(project: Project): StreamConnectionProvider = KargoConnectionProvider(project)

    override fun getServerInterface(): Class<out LanguageServer> = KargoLanguageServer::class.java

    override fun isEnabled(project: Project): Boolean = KargoSettings.getInstance().state.languageServer && Kargo.isKargoProject(project)

    override fun setEnabled(enabled: Boolean, project: Project) {
        KargoSettings.getInstance().state.languageServer = enabled
    }
}

/** Runs `kargo lsp` in the project root with the options from the settings. */
class KargoConnectionProvider(project: Project) : OSProcessStreamConnectionProvider(Kargo.commandLine(project, lspArgs())) {
    override fun getInitializationOptions(rootUri: VirtualFile?): Any = initializationOptions()

    companion object {
        fun lspArgs(): List<String> {
            val options = KargoSettings.getInstance().state
            val args = mutableListOf("lsp")
            val server = options.server ?: KargoSettings.DEFAULT_SERVER
            if (server != KargoSettings.DEFAULT_SERVER) args += listOf("--server", server)
            if (options.klibTargets) args += "--klib-targets"
            return args
        }

        fun initializationOptions(): JsonObject {
            val options = KargoSettings.getInstance().state
            return JsonObject().apply {
                addProperty("checkOnSave", options.checkOnSave)
                val targets = options.checkTargets.orEmpty().split(',').map { it.trim() }.filter { it.isNotEmpty() }
                if (targets.isNotEmpty()) add("checkTargets", JsonArray().apply { targets.forEach { add(it) } })
            }
        }
    }
}

/** `kargo lsp`'s requests beyond LSP. */
interface KargoLanguageServer : LanguageServer {
    @JsonRequest("kargo/runnables")
    fun runnables(params: RunnablesParams): CompletableFuture<List<KargoRunnable>>
}

class RunnablesParams(val textDocument: TextDocumentIdentifier?)

/** One entry of `kargo/runnables`: a program, a target's tests or a task, with its command line. */
class KargoRunnable {
    var label: String = ""
    var kind: String = ""
    var target: String? = null
    var cwd: String? = null
    var program: String? = null
    var args: List<String> = emptyList()
    var debugArgs: List<String>? = null
    var mainClass: String? = null
    var entryPoint: String? = null
    var path: String? = null
    var description: String? = null

    override fun toString() = label
}
