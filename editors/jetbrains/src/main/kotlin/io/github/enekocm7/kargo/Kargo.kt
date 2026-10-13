// Finding the `kargo` executable and the Kargo package a file belongs to, and building the
// command lines that run it.

package io.github.enekocm7.kargo

import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.configurations.PathEnvironmentVariableUtil
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.guessProjectDir
import com.intellij.openapi.util.SystemInfo
import com.intellij.openapi.vfs.VirtualFile
import io.github.enekocm7.kargo.settings.KargoSettings
import java.io.File
import java.nio.charset.StandardCharsets

object Kargo {
    const val MANIFEST = "kargo.toml"
    const val INSTALL_URL = "https://enekocm7.github.io/kargo/getting-started/installation/"

    private val exeName = if (SystemInfo.isWindows) "kargo.exe" else "kargo"

    /** The `kargo` to run: the one in the settings, else the one on `PATH`, else `~/.cargo/bin/kargo`. */
    fun executable(): File? {
        KargoSettings.getInstance().state.kargoPath?.takeIf { it.isNotBlank() }?.let { configured ->
            return File(configured).takeIf { it.isFile }
        }
        PathEnvironmentVariableUtil.findInPath(exeName)?.let { return it }
        return File(System.getProperty("user.home"), ".cargo/bin/$exeName").takeIf { it.isFile }
    }

    /** Whether the project's root holds a `kargo.toml`. */
    fun isKargoProject(project: Project): Boolean = rootManifest(project) != null

    /** The `kargo.toml` at the root of the project. */
    fun rootManifest(project: Project): VirtualFile? = project.guessProjectDir()?.findChild(MANIFEST)

    /** The directory of the package `file` belongs to: the nearest one up holding a `kargo.toml`. */
    fun packageDir(file: VirtualFile): VirtualFile? {
        var dir: VirtualFile? = if (file.isDirectory) file else file.parent
        while (dir != null) {
            if (dir.findChild(MANIFEST) != null) return dir
            dir = dir.parent
        }
        return null
    }

    /**
     * `kargo <args>` in `workDir` (default: the project root). It inherits the environment of a
     * login shell, so a `kargo` and a JDK installed by the user's shell profile are found.
     */
    fun commandLine(project: Project?, args: List<String>, workDir: String? = null): GeneralCommandLine {
        val exe = executable()?.path ?: exeName
        val dir = workDir ?: project?.guessProjectDir()?.path
        return GeneralCommandLine(listOf(exe) + args)
            .withParentEnvironmentType(GeneralCommandLine.ParentEnvironmentType.CONSOLE)
            .withCharset(StandardCharsets.UTF_8)
            .apply { if (dir != null) withWorkDirectory(dir) }
    }
}
