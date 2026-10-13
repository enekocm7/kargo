// `textDocument/hover`: the declaration under the cursor rendered as Kotlin, with its KDoc.
package io.github.enekocm7.kargo.server.features

import io.github.enekocm7.kargo.server.documents.Document
import io.github.enekocm7.kargo.server.session.AnalysisSession
import org.eclipse.lsp4j.Hover
import org.eclipse.lsp4j.MarkupContent
import org.eclipse.lsp4j.MarkupKind
import org.eclipse.lsp4j.Position
import org.jetbrains.kotlin.analysis.api.KaSession
import org.jetbrains.kotlin.analysis.api.renderer.declarations.impl.KaDeclarationRendererForSource
import org.jetbrains.kotlin.analysis.api.symbols.KaCallableSymbol
import org.jetbrains.kotlin.analysis.api.symbols.KaClassLikeSymbol
import org.jetbrains.kotlin.analysis.api.symbols.KaDeclarationSymbol
import org.jetbrains.kotlin.analysis.api.symbols.KaSymbol
import org.jetbrains.kotlin.idea.references.mainReference
import org.jetbrains.kotlin.psi.KtDeclaration
import org.jetbrains.kotlin.psi.KtFile
import org.jetbrains.kotlin.psi.KtNamedDeclaration
import org.jetbrains.kotlin.psi.KtReferenceExpression

fun hover(session: AnalysisSession, document: Document, position: Position): Hover? {
    val offset = document.lines.offset(position)
    return session.analyzeDocument(document) { file -> hoverAt(file, offset) }
}

private fun KaSession.hoverAt(file: KtFile, offset: Int): Hover? {
    val element = file.findElementAt(offset) ?: return null
    val parent = element.parent
    val symbol: KaSymbol = when {
        parent is KtReferenceExpression -> parent.mainReference.resolveToSymbol()
        parent is KtNamedDeclaration && parent.nameIdentifier == element -> parent.symbol
        else -> null
    } ?: return null
    val declaration = symbol as? KaDeclarationSymbol ?: return null
    val text = buildString {
        append("```kotlin\n")
        append(declaration.render(KaDeclarationRendererForSource.WITH_SHORT_NAMES))
        append("\n```")
        container(declaration)?.let { append("\n\n").append(it) }
        (declaration.psi as? KtDeclaration)?.docComment?.let { append("\n\n---\n\n").append(kdocText(it.text)) }
    }
    return Hover(MarkupContent(MarkupKind.MARKDOWN, text))
}

/** Where a declaration is: `in <class>` or `in package <name>`. */
private fun container(symbol: KaDeclarationSymbol): String? {
    val classId = (symbol as? KaClassLikeSymbol)?.classId
    val callableId = (symbol as? KaCallableSymbol)?.callableId
    return when {
        callableId?.classId != null -> "in `${callableId.classId!!.asFqNameString()}`"
        callableId != null && !callableId.packageName.isRoot -> "in package `${callableId.packageName.asString()}`"
        classId != null && !classId.packageFqName.isRoot -> "in package `${classId.packageFqName.asString()}`"
        else -> null
    }
}

/** A KDoc comment as Markdown: without its delimiters and leading asterisks. */
internal fun kdocText(comment: String): String =
    comment
        .removePrefix("/**")
        .removeSuffix("*/")
        .lines()
        .joinToString("\n") { it.trim().removePrefix("*").removePrefix(" ") }
        .trim()
