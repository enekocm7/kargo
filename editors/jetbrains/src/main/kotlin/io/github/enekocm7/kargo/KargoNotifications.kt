// Balloons from the plugin, and the check for a missing `kargo` when a Kargo project opens.

package io.github.enekocm7.kargo

import com.intellij.ide.BrowserUtil
import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.options.ShowSettingsUtil
import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.ProjectActivity
import io.github.enekocm7.kargo.settings.KargoConfigurable

object KargoNotifications {
    private fun group() = NotificationGroupManager.getInstance().getNotificationGroup("Kargo")

    fun error(project: Project?, title: String, content: String) = group().createNotification(title, content, NotificationType.ERROR).notify(project)

    fun info(project: Project?, title: String, content: String) = group().createNotification(title, content, NotificationType.INFORMATION).notify(project)

    fun kargoNotFound(project: Project) {
        group().createNotification(
            "Kargo is not installed",
            "This is a Kargo project, but no kargo executable was found on PATH or in ~/.cargo/bin. Install Kargo, or set its path in the settings.",
            NotificationType.WARNING,
        )
            .addAction(NotificationAction.createSimple("Install Kargo") { BrowserUtil.browse(Kargo.INSTALL_URL) })
            .addAction(NotificationAction.createSimple("Settings") { ShowSettingsUtil.getInstance().showSettingsDialog(project, KargoConfigurable::class.java) })
            .notify(project)
    }
}

/** Says so when a Kargo project opens and `kargo` can't be found. */
class KargoStartupActivity : ProjectActivity {
    override suspend fun execute(project: Project) {
        if (Kargo.isKargoProject(project) && Kargo.executable() == null) KargoNotifications.kargoNotFound(project)
    }
}
