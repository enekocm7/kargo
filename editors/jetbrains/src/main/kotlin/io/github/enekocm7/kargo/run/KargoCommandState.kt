// Starting a Kargo run configuration: the process, its console, the test tree for `kargo test`,
// and the debug port for the debugger to attach to.

package io.github.enekocm7.kargo.run

import com.intellij.execution.DefaultExecutionResult
import com.intellij.execution.ExecutionResult
import com.intellij.execution.Executor
import com.intellij.execution.configurations.CommandLineState
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.executors.DefaultDebugExecutor
import com.intellij.execution.filters.TextConsoleBuilderFactory
import com.intellij.execution.process.KillableColoredProcessHandler
import com.intellij.execution.process.ProcessHandler
import com.intellij.execution.process.ProcessTerminatedListener
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.execution.runners.ProgramRunner
import com.intellij.execution.testframework.sm.SMTestRunnerConnectionUtil
import com.intellij.util.net.NetUtils
import io.github.enekocm7.kargo.Kargo

class KargoCommandState(environment: ExecutionEnvironment, private val configuration: KargoRunConfiguration) : CommandLineState(environment) {
    /** The port `--debug` makes the JVM listen on, when running under the debugger. */
    val debugPort: Int? =
        if (environment.executor.id == DefaultDebugExecutor.EXECUTOR_ID && configuration.isDebuggable) NetUtils.findAvailableSocketPort() else null

    init {
        consoleBuilder = TextConsoleBuilderFactory.getInstance().createBuilder(environment.project).apply {
            addFilter(KargoConsoleFilter(environment.project, configuration.effectiveWorkingDirectory))
        }
    }

    /** The command line, with the flags the IDE needs added right after the subcommand. */
    fun commandLine(): GeneralCommandLine {
        val args = kargoArgs(configuration.args, configuration.isTest, debugPort)
        return Kargo.commandLine(environment.project, args, configuration.effectiveWorkingDirectory).apply {
            withEnvironment(configuration.env.envs)
            if (!configuration.env.isPassParentEnvs) withParentEnvironmentType(GeneralCommandLine.ParentEnvironmentType.NONE)
        }
    }

    override fun startProcess(): ProcessHandler {
        val handler = KillableColoredProcessHandler(commandLine())
        ProcessTerminatedListener.attach(handler)
        return handler
    }

    override fun execute(executor: Executor, runner: ProgramRunner<*>): ExecutionResult {
        if (!configuration.isTest) return super.execute(executor, runner)
        val handler = startProcess()
        val properties = KargoTestConsoleProperties(configuration, executor)
        val console = SMTestRunnerConnectionUtil.createAndAttachConsole("Kargo", handler, properties)
        console.addMessageFilter(KargoConsoleFilter(environment.project, configuration.effectiveWorkingDirectory))
        return DefaultExecutionResult(console, handler, *createActions(console, handler, executor))
    }

    companion object {
        /**
         * `args` with what the IDE needs: `--message-format json` for the test tree, and
         * `--debug=<port>` (on the JVM target unless one is chosen) under the debugger.
         */
        fun kargoArgs(args: List<String>, test: Boolean, debugPort: Int?): List<String> {
            val at = args.indexOfFirst { !it.startsWith("-") }
            if (at < 0) return args
            // Arguments after `--` belong to the program.
            val end = args.indexOf("--").takeIf { it >= 0 } ?: args.size
            val own = args.subList(0, end)
            val extra = mutableListOf<String>()
            if (test && own.none { it == "--message-format" || it.startsWith("--message-format=") }) {
                extra += listOf("--message-format", "json")
            }
            if (debugPort != null) {
                extra += "--debug=$debugPort"
                if (own.none { it == "-t" || it == "--target" || it.startsWith("--target=") || (it.startsWith("-t") && it.length > 2) }) {
                    extra += listOf("-t", "jvm")
                }
            }
            return args.subList(0, at + 1) + extra + args.subList(at + 1, args.size)
        }
    }
}
