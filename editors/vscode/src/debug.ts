//! Debugging through `kargo dap`: the `kargo` debug type of launch.json, its starting configurations,
//! and the adapter that runs `kargo dap` in the project.

import * as vscode from 'vscode';
import { kargoBinary } from './project';

/** The debug type of Kargo's configurations in launch.json. */
export const DEBUG_TYPE = 'kargo';

export class KargoDebugConfigurations implements vscode.DebugConfigurationProvider {
    /** The configurations offered when launch.json has none: run the program, run the tests, attach. */
    provideDebugConfigurations(): vscode.DebugConfiguration[] {
        return [
            { type: DEBUG_TYPE, request: 'launch', name: 'Kargo: run the program', args: ['run'] },
            { type: DEBUG_TYPE, request: 'launch', name: 'Kargo: run the tests', args: ['test'] },
            { type: DEBUG_TYPE, request: 'attach', name: 'Kargo: attach to port 5005', port: 5005 },
        ];
    }

    /** Fills in what a launch configuration leaves out: a launch runs the program. */
    resolveDebugConfiguration(_folder: vscode.WorkspaceFolder | undefined, config: vscode.DebugConfiguration): vscode.DebugConfiguration {
        if (config.request === undefined) config.request = 'launch';
        if (config.request === 'launch' && !Array.isArray(config.args)) config.args = ['run'];
        return config;
    }
}

/** Runs `kargo dap` in the project of the debug session. */
export class KargoDebugAdapters implements vscode.DebugAdapterDescriptorFactory {
    createDebugAdapterDescriptor(session: vscode.DebugSession): vscode.DebugAdapterDescriptor | undefined {
        let program: string;
        try {
            program = kargoBinary();
        } catch (error) {
            void vscode.window.showErrorMessage(`${(error as Error).message} Kargo can't debug without it.`);
            return undefined;
        }
        const cwd = session.workspaceFolder?.uri.fsPath ?? vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
        return new vscode.DebugAdapterExecutable(program, ['dap'], { cwd });
    }
}

/** Starts a debug session for a `kargo` command, such as `["test", "com.example.MathTest"]`. */
export function debugKargo(folder: vscode.WorkspaceFolder, label: string, args: readonly string[]): Thenable<boolean> {
    return vscode.debug.startDebugging(folder, { type: DEBUG_TYPE, request: 'launch', name: label, args: [...args] });
}
