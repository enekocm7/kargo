// The Kargo run configuration: any `kargo` command line, run in a package directory.

package io.github.enekocm7.kargo.run

import com.intellij.execution.Executor
import com.intellij.execution.configuration.EnvironmentVariablesData
import com.intellij.execution.configurations.ConfigurationFactory
import com.intellij.execution.configurations.ConfigurationTypeBase
import com.intellij.execution.configurations.ConfigurationTypeUtil
import com.intellij.execution.configurations.LocatableConfigurationBase
import com.intellij.execution.configurations.RunConfiguration
import com.intellij.execution.configurations.RunProfileState
import com.intellij.execution.configurations.RuntimeConfigurationError
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.openapi.options.SettingsEditor
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.guessProjectDir
import com.intellij.openapi.util.JDOMExternalizerUtil
import com.intellij.util.execution.ParametersListUtil
import io.github.enekocm7.kargo.KargoIcons
import org.jdom.Element

class KargoConfigurationType : ConfigurationTypeBase(ID, "Kargo", "Run a Kargo command", KargoIcons.KARGO) {
    init {
        addFactory(KargoConfigurationFactory(this))
    }

    val factory: ConfigurationFactory get() = configurationFactories.single()

    companion object {
        const val ID = "KargoRunConfiguration"

        fun getInstance(): KargoConfigurationType = ConfigurationTypeUtil.findConfigurationType(KargoConfigurationType::class.java)
    }
}

class KargoConfigurationFactory(type: KargoConfigurationType) : ConfigurationFactory(type) {
    override fun getId() = KargoConfigurationType.ID

    override fun createTemplateConfiguration(project: Project): RunConfiguration = KargoRunConfiguration(project, this, "Kargo")
}

class KargoRunConfiguration(project: Project, factory: ConfigurationFactory, name: String) :
    LocatableConfigurationBase<RunProfileState>(project, factory, name) {

    /** What follows `kargo`, such as `run -t jvm` or `test MathTest`. */
    var command: String = "run"

    /** Where `kargo` runs; empty: the project root. */
    var workingDirectory: String? = null

    var env: EnvironmentVariablesData = EnvironmentVariablesData.DEFAULT

    val args: List<String> get() = ParametersListUtil.parse(command)

    /** The subcommand, such as `run` or `test`. */
    val subcommand: String? get() = args.firstOrNull { !it.startsWith("-") }

    val isTest: Boolean get() = subcommand == "test"

    /** Whether the command can start a JVM under a debugger (`kargo run` and `kargo test` take `--debug`). */
    val isDebuggable: Boolean get() = subcommand == "run" || subcommand == "test"

    val effectiveWorkingDirectory: String? get() = workingDirectory?.takeIf { it.isNotBlank() } ?: project.guessProjectDir()?.path

    override fun getConfigurationEditor(): SettingsEditor<out RunConfiguration> = KargoRunConfigurationEditor(project)

    override fun getState(executor: Executor, environment: ExecutionEnvironment): RunProfileState = KargoCommandState(environment, this)

    override fun checkConfiguration() {
        if (subcommand == null) throw RuntimeConfigurationError("Enter a Kargo command, such as run or test")
    }

    override fun suggestedName(): String = "kargo $command"

    override fun readExternal(element: Element) {
        super.readExternal(element)
        command = JDOMExternalizerUtil.readField(element, "command") ?: "run"
        workingDirectory = JDOMExternalizerUtil.readField(element, "workingDirectory")
        env = EnvironmentVariablesData.readExternal(element)
    }

    override fun writeExternal(element: Element) {
        super.writeExternal(element)
        JDOMExternalizerUtil.writeField(element, "command", command)
        JDOMExternalizerUtil.writeField(element, "workingDirectory", workingDirectory)
        env.writeExternal(element)
    }
}
