// Kargo's Kotlin language server: LSP over stdin and stdout, started by `kargo lsp --server kargo`.
package io.github.enekocm7.kargo.server

import io.github.enekocm7.kargo.server.server.KargoLanguageClient
import io.github.enekocm7.kargo.server.server.KargoLanguageServer
import org.eclipse.lsp4j.jsonrpc.Launcher
import kotlin.system.exitProcess

fun main(args: Array<String>) {
    if ("--version" in args) {
        println("kargo-kotlin-server ${KargoLanguageServer.VERSION}")
        return
    }
    // stdout carries the protocol: anything else printed there (IntelliJ core logs to it) goes to stderr.
    val protocol = System.out
    System.setOut(System.err)
    val server = KargoLanguageServer(onExit = { code -> exitProcess(code) })
    val launcher = Launcher.Builder<KargoLanguageClient>()
        .setLocalService(server)
        .setRemoteInterface(KargoLanguageClient::class.java)
        .setInput(System.`in`)
        .setOutput(protocol)
        .create()
    server.connect(launcher.remoteProxy)
    launcher.startListening().get()
    // The editor closed the stream without `exit`.
    exitProcess(1)
}
