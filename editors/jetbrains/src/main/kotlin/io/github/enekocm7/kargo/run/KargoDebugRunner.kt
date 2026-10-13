// Debugging `kargo run` and `kargo test`: Kargo starts the JVM suspended with `--debug=<port>`,
// and the IDE's Java debugger attaches to that port once it listens.

package io.github.enekocm7.kargo.run

import com.intellij.debugger.impl.GenericDebuggerRunner
import com.intellij.execution.configurations.RemoteConnection
import com.intellij.execution.configurations.RunProfile
import com.intellij.execution.configurations.RunProfileState
import com.intellij.execution.executors.DefaultDebugExecutor
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.execution.ui.RunContentDescriptor

class KargoDebugRunner : GenericDebuggerRunner() {
    override fun getRunnerId() = "KargoDebugRunner"

    override fun canRun(executorId: String, profile: RunProfile): Boolean =
        executorId == DefaultDebugExecutor.EXECUTOR_ID && profile is KargoRunConfiguration && profile.isDebuggable

    override fun createContentDescriptor(state: RunProfileState, environment: ExecutionEnvironment): RunContentDescriptor? {
        val port = (state as? KargoCommandState)?.debugPort ?: return null
        // Poll: the JVM only listens once Kargo has compiled the program.
        val connection = RemoteConnection(true, "127.0.0.1", port.toString(), false)
        return attachVirtualMachine(state, environment, connection, true)
    }
}
