// Run and Debug icons in the gutter of `fun main`, test functions and test classes in a Kargo
// package, and a Render icon on `@Preview` composables (`kargo preview`). Kargo's sources are not part of the IDE's Kotlin model, so declarations are recognized
// from the PSI tree's tokens rather than from Kotlin's resolved PSI.

package io.github.enekocm7.kargo.run

import com.intellij.execution.ProgramRunnerUtil
import com.intellij.execution.RunManager
import com.intellij.execution.executors.DefaultDebugExecutor
import com.intellij.execution.executors.DefaultRunExecutor
import com.intellij.execution.Executor
import com.intellij.execution.lineMarker.RunLineMarkerContributor
import com.intellij.icons.AllIcons
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VfsUtilCore
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiFile
import com.intellij.psi.PsiNamedElement
import com.intellij.psi.util.PsiTreeUtil
import com.intellij.util.execution.ParametersListUtil
import io.github.enekocm7.kargo.Kargo
import io.github.enekocm7.kargo.lsp.KargoLsp
import java.io.File
import java.util.concurrent.TimeUnit

class KargoRunLineMarkerContributor : RunLineMarkerContributor() {
    override fun getInfo(element: PsiElement): Info? {
        if (element.firstChild != null || element.node.elementType.toString() != "IDENTIFIER") return null
        val keyword = PsiTreeUtil.prevVisibleLeaf(element)?.text ?: return null
        if (keyword != "fun" && keyword != "class" && keyword != "object") return null
        val file = element.containingFile?.virtualFile ?: return null
        val packageDir = Kargo.packageDir(file) ?: return null
        val declaration = element.parent ?: return null
        val spec = when {
            keyword == "fun" && element.text == "main" && declaration.parent == element.containingFile ->
                RunSpec.Main(file, packageDir)
            keyword == "fun" && TEST_ANNOTATION.containsMatchIn(declaration.text.substring(0, element.startOffsetInParent)) ->
                RunSpec.Test(file, packageDir, testFilter(element, declaration, method = element.text.removeSurrounding("`")))
            keyword != "fun" && TEST_ANNOTATION.containsMatchIn(declaration.text) && isInTests(file, packageDir) ->
                RunSpec.Test(file, packageDir, testFilter(element, declaration, method = null))
            keyword == "fun" && declaration.parent == element.containingFile &&
                PREVIEW_ANNOTATION.containsMatchIn(declaration.text.substring(0, element.startOffsetInParent)) -> {
                val packageName = PACKAGE.find(element.containingFile.text)?.groupValues?.get(1)
                val spec = RunSpec.Preview(file, packageDir, listOfNotNull(packageName, element.text).joinToString("."))
                val render = KargoRunAction(spec, DefaultRunExecutor.getRunExecutorInstance())
                return Info(AllIcons.Actions.Preview, arrayOf<AnAction>(render)) { "Render ${spec.label()} with kargo preview" }
            }
            else -> return null
        }
        val actions = arrayOf<AnAction>(
            KargoRunAction(spec, DefaultRunExecutor.getRunExecutorInstance()),
            KargoRunAction(spec, DefaultDebugExecutor.getDebugExecutorInstance()),
        )
        return Info(AllIcons.RunConfigurations.TestState.Run, actions) { "Run ${spec.label()} with Kargo" }
    }

    private fun isInTests(file: VirtualFile, packageDir: VirtualFile): Boolean =
        VfsUtilCore.getRelativePath(file, packageDir)?.substringBefore('/')?.startsWith("tests") == true

    /** `package.Class.method`: what `kargo test` filters on. */
    private fun testFilter(identifier: PsiElement, declaration: PsiElement, method: String?): String {
        val classes = generateSequence(if (method == null) declaration else declaration.parent) { it.parent }
            .takeWhile { it !is PsiFile }
            .filter { it.node?.elementType?.toString() in CLASS_TYPES }
            .mapNotNull { (it as? PsiNamedElement)?.name }
            .toList()
            .reversed()
        val packageName = PACKAGE.find(identifier.containingFile.text)?.groupValues?.get(1)
        return (listOfNotNull(packageName) + classes + listOfNotNull(method)).joinToString(".")
    }

    companion object {
        private val TEST_ANNOTATION = Regex("""@(?:[\w.]+\.)?(?:Test|ParameterizedTest|RepeatedTest|TestFactory)\b""")
        private val PREVIEW_ANNOTATION = Regex("""@(?:[\w.]+\.)?Preview\b""")
        private val PACKAGE = Regex("""^\s*package\s+([\w.]+)""", RegexOption.MULTILINE)
        private val CLASS_TYPES = setOf("CLASS", "OBJECT_DECLARATION")
    }
}

/** What a gutter icon runs. */
sealed class RunSpec(val file: VirtualFile, val packageDir: VirtualFile) {
    class Main(file: VirtualFile, packageDir: VirtualFile) : RunSpec(file, packageDir)
    class Test(file: VirtualFile, packageDir: VirtualFile, val filter: String) : RunSpec(file, packageDir)
    /** A `@Preview` composable, by its qualified name. */
    class Preview(file: VirtualFile, packageDir: VirtualFile, val name: String) : RunSpec(file, packageDir)

    fun label(): String = when (this) {
        is Main -> file.nameWithoutExtension
        is Preview -> name.substringAfterLast('.')
        is Test -> filter.substringAfterLast('.').let { last -> if (last.firstOrNull()?.isUpperCase() == true) last else filter.split('.').takeLast(2).joinToString(".") }
    }

    /** The target a source directory belongs to by its `src-<target>` name, or null for common code.
     * Custom `[layout]` directories have no suffix, so they read as common; `kargo metadata`
     * already lists them in the right `source-roots`. */
    fun target(): String? {
        val top = VfsUtilCore.getRelativePath(file, packageDir)?.substringBefore('/') ?: return null
        return top.substringAfter('-', "").takeIf { it in setOf("jvm", "wasm", "native", "android") }
    }

    /** The `kargo` arguments, asking `kargo lsp` which program a source file is when it can tell. */
    fun args(project: Project): List<String> = when (this) {
        is Test -> listOfNotNull("test", target()?.let { "-t" }, target(), filter)
        is Preview -> listOf("preview", name)
        is Main -> {
            val path = File(file.path).canonicalPath
            val runnables = runCatching { KargoLsp.runnables(project, file).get(5, TimeUnit.SECONDS) }.getOrDefault(emptyList())
            val program = runnables.firstOrNull { it.kind == "run" && it.path != null && File(it.path!!).canonicalPath == path }
            when {
                program != null -> program.args
                VfsUtilCore.getRelativePath(file, packageDir)?.startsWith("examples/") == true -> listOf("run", "--example", file.nameWithoutExtension)
                else -> listOf("run", "-t", target() ?: "jvm")
            }
        }
    }
}

/** Runs or debugs a [RunSpec] through a temporary Kargo run configuration. */
class KargoRunAction(private val spec: RunSpec, private val executor: Executor) :
    AnAction("${executor.actionName} '${spec.label()}'", null, executor.icon) {

    override fun getActionUpdateThread() = ActionUpdateThread.BGT

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val args = ProgressManager.getInstance().runProcessWithProgressSynchronously<List<String>, RuntimeException>(
            { spec.args(project) }, "Finding Kargo program", true, project,
        )
        runKargo(project, spec.label(), args, spec.packageDir.path, executor)
    }
}

/** Runs `kargo <args>` in `workDir` as a temporary run configuration named `name`, and selects it. */
fun runKargo(project: Project, name: String, args: List<String>, workDir: String?, executor: Executor = DefaultRunExecutor.getRunExecutorInstance()) {
    val runManager = RunManager.getInstance(project)
    val settings = runManager.createConfiguration(name, KargoConfigurationType.getInstance().factory)
    (settings.configuration as KargoRunConfiguration).apply {
        command = ParametersListUtil.join(args)
        workingDirectory = workDir
    }
    runManager.setTemporaryConfiguration(settings)
    runManager.selectedConfiguration = settings
    ProgramRunnerUtil.executeConfiguration(settings, executor)
}
