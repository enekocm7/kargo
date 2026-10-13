// The Kargo tool window: the project's programs, tests and tasks from `kargo/runnables`, to run or
// debug with a double-click or the toolbar.

package io.github.enekocm7.kargo.toolwindow

import com.intellij.execution.Executor
import com.intellij.execution.executors.DefaultDebugExecutor
import com.intellij.execution.executors.DefaultRunExecutor
import com.intellij.icons.AllIcons
import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.SimpleToolWindowPanel
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.ui.ColoredTreeCellRenderer
import com.intellij.ui.ScrollPaneFactory
import com.intellij.ui.SimpleTextAttributes
import com.intellij.ui.content.ContentFactory
import com.intellij.ui.treeStructure.Tree
import io.github.enekocm7.kargo.Kargo
import io.github.enekocm7.kargo.KargoIcons
import io.github.enekocm7.kargo.lsp.KargoLsp
import io.github.enekocm7.kargo.lsp.KargoRunnable
import io.github.enekocm7.kargo.run.runKargo
import java.awt.event.MouseAdapter
import java.awt.event.MouseEvent
import javax.swing.JTree
import javax.swing.tree.DefaultMutableTreeNode
import javax.swing.tree.DefaultTreeModel

class KargoToolWindowFactory : ToolWindowFactory {
    override fun shouldBeAvailable(project: Project): Boolean = Kargo.isKargoProject(project)

    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val panel = KargoRunnablesPanel(project)
        toolWindow.contentManager.addContent(ContentFactory.getInstance().createContent(panel, null, false))
        panel.refresh()
    }
}

class KargoRunnablesPanel(private val project: Project) : SimpleToolWindowPanel(true, true) {
    private val root = DefaultMutableTreeNode("Kargo")
    private val model = DefaultTreeModel(root)
    private val tree = Tree(model).apply {
        isRootVisible = false
        cellRenderer = Renderer()
        emptyText.text = "Open a Kotlin file or kargo.toml to start kargo lsp, then refresh"
    }

    init {
        tree.addMouseListener(object : MouseAdapter() {
            override fun mouseClicked(e: MouseEvent) {
                if (e.clickCount == 2) run(DefaultRunExecutor.getRunExecutorInstance())
            }
        })
        val actions = DefaultActionGroup(
            simpleAction("Refresh", AllIcons.Actions.Refresh) { refresh() },
            simpleAction("Run", AllIcons.Actions.Execute) { run(DefaultRunExecutor.getRunExecutorInstance()) },
            simpleAction("Debug", AllIcons.Actions.StartDebugger) { run(DefaultDebugExecutor.getDebugExecutorInstance()) },
        )
        toolbar = ActionManager.getInstance().createActionToolbar("KargoToolWindow", actions, true).apply { targetComponent = tree }.component
        setContent(ScrollPaneFactory.createScrollPane(tree))
    }

    private fun simpleAction(text: String, icon: javax.swing.Icon, perform: () -> Unit) = object : DumbAwareAction(text, null, icon) {
        override fun getActionUpdateThread() = ActionUpdateThread.EDT

        override fun actionPerformed(e: AnActionEvent) = perform()
    }

    fun refresh() {
        KargoLsp.runnables(project, null).whenComplete { runnables, _ ->
            ApplicationManager.getApplication().invokeLater { show(runnables.orEmpty()) }
        }
    }

    private fun show(runnables: List<KargoRunnable>) {
        root.removeAllChildren()
        for ((kind, title) in listOf("run" to "Programs", "test" to "Tests", "task" to "Tasks")) {
            val entries = runnables.filter { it.kind == kind }
            if (entries.isEmpty()) continue
            val group = DefaultMutableTreeNode(title)
            entries.forEach { group.add(DefaultMutableTreeNode(it)) }
            root.add(group)
        }
        model.reload()
        for (row in 0 until tree.rowCount) tree.expandRow(row)
    }

    private fun run(executor: Executor) {
        val runnable = (tree.lastSelectedPathComponent as? DefaultMutableTreeNode)?.userObject as? KargoRunnable ?: return
        runKargo(project, runnable.label, runnable.args, runnable.cwd, executor)
    }

    private class Renderer : ColoredTreeCellRenderer() {
        override fun customizeCellRenderer(tree: JTree, value: Any?, selected: Boolean, expanded: Boolean, leaf: Boolean, row: Int, hasFocus: Boolean) {
            val node = (value as? DefaultMutableTreeNode)?.userObject
            if (node !is KargoRunnable) {
                icon = AllIcons.Nodes.Folder
                append(node.toString(), SimpleTextAttributes.REGULAR_BOLD_ATTRIBUTES)
                return
            }
            icon = when (node.kind) {
                "test" -> AllIcons.RunConfigurations.Junit
                "task" -> KargoIcons.KARGO
                else -> AllIcons.RunConfigurations.Application
            }
            append(node.label)
            val detail = node.description ?: node.mainClass ?: node.entryPoint
            if (detail != null) append("  $detail", SimpleTextAttributes.GRAYED_ATTRIBUTES)
        }
    }
}
