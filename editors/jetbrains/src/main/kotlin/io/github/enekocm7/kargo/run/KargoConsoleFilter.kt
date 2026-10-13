// Turns the `src/Main.kt:2:16: error: ...` locations Kargo and the Kotlin compiler print into links.

package io.github.enekocm7.kargo.run

import com.intellij.execution.filters.Filter
import com.intellij.execution.filters.OpenFileHyperlinkInfo
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.LocalFileSystem
import java.io.File

class KargoConsoleFilter(private val project: Project, private val baseDir: String?) : Filter {
    override fun applyFilter(line: String, entireLength: Int): Filter.Result? {
        val match = LOCATION.find(line) ?: return null
        val (path, lineNumber, column) = match.destructured
        val file = File(path).let { if (it.isAbsolute || baseDir == null) it else File(baseDir, path) }
        val virtualFile = LocalFileSystem.getInstance().findFileByIoFile(file) ?: return null
        val start = entireLength - line.length + match.groups[1]!!.range.first
        val end = entireLength - line.length + match.range.last + 1
        val info = OpenFileHyperlinkInfo(project, virtualFile, lineNumber.toInt() - 1, (column.toIntOrNull() ?: 1) - 1)
        return Filter.Result(start, end, info)
    }

    companion object {
        /** `path.kt:line[:column]`, the path absolute (with a drive letter on Windows) or relative. */
        val LOCATION = Regex("""(?:^|[\s(])((?:[A-Za-z]:)?[^\s:()]+\.(?:kt|kts|java|toml)):(\d+)(?::(\d+))?""")
    }
}
