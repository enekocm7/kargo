// The LSP layer: lifecycle, document sync, the features, and Kargo's extensions
// (`kargo/projectChanged` from `kargo lsp`, `kargo/status` to it).
//
// All analysis runs on one thread, in the order requests and edits arrive. The session is built
// there too and replaced when the project changes or a file is saved, so a request never sees a
// session that is being built.
package io.github.enekocm7.kargo.server.server

import com.google.gson.JsonElement
import com.google.gson.JsonObject
import io.github.enekocm7.kargo.server.documents.Documents
import io.github.enekocm7.kargo.server.features.diagnostics
import io.github.enekocm7.kargo.server.features.hover
import io.github.enekocm7.kargo.server.log.Log
import io.github.enekocm7.kargo.server.model.ProjectModel
import io.github.enekocm7.kargo.server.session.AnalysisSession
import org.eclipse.lsp4j.DidChangeConfigurationParams
import org.eclipse.lsp4j.DidChangeTextDocumentParams
import org.eclipse.lsp4j.DidChangeWatchedFilesParams
import org.eclipse.lsp4j.DidCloseTextDocumentParams
import org.eclipse.lsp4j.DidOpenTextDocumentParams
import org.eclipse.lsp4j.DidSaveTextDocumentParams
import org.eclipse.lsp4j.Hover
import org.eclipse.lsp4j.HoverParams
import org.eclipse.lsp4j.InitializeParams
import org.eclipse.lsp4j.InitializeResult
import org.eclipse.lsp4j.MessageParams
import org.eclipse.lsp4j.MessageType
import org.eclipse.lsp4j.PublishDiagnosticsParams
import org.eclipse.lsp4j.SaveOptions
import org.eclipse.lsp4j.ServerCapabilities
import org.eclipse.lsp4j.ServerInfo
import org.eclipse.lsp4j.TextDocumentSyncKind
import org.eclipse.lsp4j.TextDocumentSyncOptions
import org.eclipse.lsp4j.jsonrpc.messages.Either
import org.eclipse.lsp4j.jsonrpc.services.JsonNotification
import org.eclipse.lsp4j.services.LanguageClient
import org.eclipse.lsp4j.services.LanguageClientAware
import org.eclipse.lsp4j.services.LanguageServer
import org.eclipse.lsp4j.services.TextDocumentService
import org.eclipse.lsp4j.services.WorkspaceService
import org.jetbrains.kotlin.config.KotlinCompilerVersion
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/** What `kargo lsp` knows of the server's state, sent as `kargo/status`. */
data class StatusParams(val state: String, val message: String? = null)

/** The editor's side, with Kargo's notification. */
interface KargoLanguageClient : LanguageClient {
    /** `indexing`, `ready` or `degraded`; `kargo lsp` shows it as `$/progress`. */
    @JsonNotification("kargo/status")
    fun status(params: StatusParams)
}

class KargoLanguageServer(private val onExit: (Int) -> Unit) : LanguageServer, LanguageClientAware {
    companion object {
        const val VERSION = "0.1.0"

        /** How long diagnostics wait for the next keystroke. */
        const val DIAGNOSTICS_DELAY_MS = 150L
    }

    private lateinit var client: KargoLanguageClient
    private val documents = Documents()
    private val analysis = Executors.newSingleThreadScheduledExecutor { Thread(it, "kargo-analysis").apply { isDaemon = true } }
    private val pendingDiagnostics = ConcurrentHashMap<String, ScheduledFuture<*>>()

    /** Only touched on the analysis thread. */
    private var session: AnalysisSession? = null
    private var model: ProjectModel? = null
    private var shutdown = false

    override fun connect(client: LanguageClient) {
        this.client = client as KargoLanguageClient
    }

    override fun initialize(params: InitializeParams): CompletableFuture<InitializeResult> {
        Log.verbose = params.trace == "verbose"
        val project = (params.initializationOptions as? JsonObject)?.getAsJsonObject("kargo")?.get("project")
        project?.let { onAnalysisThread { load(it) } }
        if (project == null) {
            Log.info("no project model in the initialize request; run the server through `kargo lsp --server kargo`")
        }
        val capabilities = ServerCapabilities().apply {
            setTextDocumentSync(
                TextDocumentSyncOptions().apply {
                    openClose = true
                    change = TextDocumentSyncKind.Incremental
                    setSave(SaveOptions(false))
                },
            )
            setHoverProvider(true)
        }
        return CompletableFuture.completedFuture(InitializeResult(capabilities, ServerInfo("kargo-kotlin-server", VERSION)))
    }

    override fun shutdown(): CompletableFuture<Any> = CompletableFuture.supplyAsync({
        shutdown = true
        session?.close()
        session = null
        null
    }, analysis)

    override fun exit() {
        onExit(if (shutdown) 0 else 1)
    }

    /** `kargo lsp` imported the project again. */
    @JsonNotification("kargo/projectChanged")
    fun projectChanged(params: JsonObject) {
        val project = params.get("project") ?: return
        onAnalysisThread { load(project) }
    }

    override fun getTextDocumentService(): TextDocumentService = textDocuments

    override fun getWorkspaceService(): WorkspaceService = workspace

    /** Builds the session for a model, replacing the current one. On the analysis thread. */
    private fun load(json: JsonElement) {
        val model = try {
            ProjectModel.parse(json.asJsonObject)
        } catch (e: Exception) {
            Log.error("could not read the project model", e)
            status("degraded", e.message ?: "the project model is invalid")
            return
        }
        this.model = model
        Log.to(model.logFile)
        warnAboutKotlinVersion(model)
        rebuild("${model.modules.size} modules, ${model.libraries.size} libraries")
    }

    /** Builds the session again from the current model, then analyzes the open documents. */
    private fun rebuild(what: String) {
        val model = model ?: return
        status("indexing", what)
        val started = System.nanoTime()
        val built = try {
            AnalysisSession.build(model)
        } catch (e: Throwable) {
            Log.error("could not build the analysis session", e)
            status("degraded", "the project could not be analyzed: ${e.message}")
            return
        }
        session?.close()
        session = built
        Log.info("analysis session: ${built.fileCount} files, built in ${(System.nanoTime() - started) / 1_000_000} ms")
        status("ready")
        documents.all().forEach { publishDiagnostics(it.uri) }
    }

    private fun warnAboutKotlinVersion(model: ProjectModel) {
        val project = model.kotlinVersion ?: return
        val ours = KotlinCompilerVersion.VERSION
        if (minorVersion(project) > minorVersion(ours)) {
            val text = "kargo lsp analyzes with Kotlin ${ours.substringBefore('-')}; this project uses $project; update Kargo for full support"
            client.showMessage(MessageParams(MessageType.Warning, text))
        }
    }

    private fun status(state: String, message: String? = null) {
        if (::client.isInitialized) client.status(StatusParams(state, message))
    }

    private fun onAnalysisThread(action: () -> Unit) {
        analysis.execute {
            try {
                action()
            } catch (e: Throwable) {
                Log.error("analysis failed", e)
            }
        }
    }

    private fun <T> onAnalysisThread(default: T, action: (AnalysisSession) -> T): CompletableFuture<T> =
        CompletableFuture.supplyAsync({
            val session = session ?: return@supplyAsync default
            try {
                action(session)
            } catch (e: Throwable) {
                Log.error("request failed", e)
                default
            }
        }, analysis)

    /** Publishes the diagnostics of an open document after [DIAGNOSTICS_DELAY_MS], unless it changes again. */
    private fun scheduleDiagnostics(uri: String) {
        val next = analysis.schedule({ publishDiagnostics(uri) }, DIAGNOSTICS_DELAY_MS, TimeUnit.MILLISECONDS)
        pendingDiagnostics.put(uri, next)?.cancel(false)
    }

    /** On the analysis thread. */
    private fun publishDiagnostics(uri: String) {
        val session = session ?: return
        val document = documents[uri] ?: return
        val found = try {
            diagnostics(session, document)
        } catch (e: Throwable) {
            Log.error("diagnostics of $uri failed", e)
            return
        }
        client.publishDiagnostics(PublishDiagnosticsParams(uri, found, document.version))
    }

    private val textDocuments = object : TextDocumentService {
        override fun didOpen(params: DidOpenTextDocumentParams) {
            val doc = params.textDocument
            documents.open(doc.uri, doc.version, doc.text)
            scheduleDiagnostics(doc.uri)
        }

        override fun didChange(params: DidChangeTextDocumentParams) {
            val uri = params.textDocument.uri
            documents.change(uri, params.textDocument.version, params.contentChanges)
            scheduleDiagnostics(uri)
        }

        override fun didClose(params: DidCloseTextDocumentParams) {
            val uri = params.textDocument.uri
            documents.close(uri)
            pendingDiagnostics.remove(uri)?.cancel(false)
            client.publishDiagnostics(PublishDiagnosticsParams(uri, emptyList()))
        }

        override fun didSave(params: DidSaveTextDocumentParams) {
            // Other files see the saved contents once the session is built again.
            onAnalysisThread { rebuild("saved ${params.textDocument.uri.substringAfterLast('/')}") }
        }

        override fun hover(params: HoverParams): CompletableFuture<Hover?> {
            val document = documents[params.textDocument.uri] ?: return CompletableFuture.completedFuture(null)
            return onAnalysisThread(null) { session -> hover(session, document, params.position) }
        }
    }

    private val workspace = object : WorkspaceService {
        override fun didChangeConfiguration(params: DidChangeConfigurationParams) {}

        override fun didChangeWatchedFiles(params: DidChangeWatchedFilesParams) {
            // Files created, deleted or changed outside the editor.
            if (params.changes.any { change -> documents[change.uri] == null }) {
                onAnalysisThread { rebuild("${params.changes.size} changed files") }
            }
        }
    }
}

/** `major * 1000 + minor` of a Kotlin version such as `2.4.21` or `2.4.20-dev-6138`. */
internal fun minorVersion(version: String): Int {
    val parts = version.split('.', '-').mapNotNull(String::toIntOrNull)
    return (parts.getOrElse(0) { 0 }) * 1000 + parts.getOrElse(1) { 0 }
}
