// `kargo lsp` as an LSP4IJ language server: how it starts and the requests Kargo adds to LSP. The IDE
// keeps its own Kotlin support, so the server only handles `kargo.toml`.

package io.github.enekocm7.kargo.lsp

import com.intellij.openapi.project.Project
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

/**
 * Runs `kargo lsp` in the project root without a Kotlin server (`--server none`): completion, hover,
 * validation and quick fixes for `kargo.toml`, plus `kargo/runnables` and the `kargo/addDependency` and `kargo/addTarget` commands.
 */
class KargoConnectionProvider(project: Project) : OSProcessStreamConnectionProvider(Kargo.commandLine(project, listOf("lsp", "--server", "none")))

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
