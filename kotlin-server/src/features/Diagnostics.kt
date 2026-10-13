// `textDocument/publishDiagnostics`: the compiler frontend's errors and warnings for an open
// document.
package io.github.enekocm7.kargo.server.features

import io.github.enekocm7.kargo.server.documents.Document
import io.github.enekocm7.kargo.server.session.AnalysisSession
import org.eclipse.lsp4j.Diagnostic
import org.eclipse.lsp4j.DiagnosticSeverity
import org.eclipse.lsp4j.DiagnosticTag
import org.eclipse.lsp4j.jsonrpc.messages.Either
import org.jetbrains.kotlin.analysis.api.components.KaDiagnosticCheckerFilter
import org.jetbrains.kotlin.analysis.api.diagnostics.KaSeverity

/** Diagnostic names whose code is unused or deprecated, which editors show faded or struck out. */
private val UNNECESSARY = setOf("UNUSED_VARIABLE", "UNUSED_PARAMETER", "UNUSED_ANONYMOUS_PARAMETER", "UNUSED_EXPRESSION")

/** The diagnostics of an open document, or an empty list when it is in no module. */
fun diagnostics(session: AnalysisSession, document: Document): List<Diagnostic> =
    session.analyzeDocument(document) { file ->
        file.collectDiagnostics(KaDiagnosticCheckerFilter.ONLY_COMMON_CHECKERS).flatMap { diagnostic ->
            diagnostic.textRanges.take(1).map { range ->
                Diagnostic().apply {
                    this.range = document.lines.range(range.startOffset, range.endOffset)
                    severity = when (diagnostic.severity) {
                        KaSeverity.ERROR -> DiagnosticSeverity.Error
                        KaSeverity.WARNING -> DiagnosticSeverity.Warning
                        KaSeverity.INFO -> DiagnosticSeverity.Information
                    }
                    code = Either.forLeft(diagnostic.factoryName)
                    source = "kotlin"
                    message = Either.forLeft(diagnostic.defaultMessage)
                    tags = buildList {
                        if (diagnostic.factoryName in UNNECESSARY) add(DiagnosticTag.Unnecessary)
                        if (diagnostic.factoryName.startsWith("DEPRECATION")) add(DiagnosticTag.Deprecated)
                    }.ifEmpty { null }
                }
            }
        }
    } ?: emptyList()
