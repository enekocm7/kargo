//! The Kargo extension for VS Code: `kargo lsp` for Kotlin and `kargo.toml`, the `kargo` tasks with
//! their compiler messages in the Problems view, a test explorer, Run and Debug buttons over `fun
//! main` and the tests, debugging through `kargo dap`, and a status bar for the target and profile.
//!
//! The extension activates in a folder with a `kargo.toml` at its root. Each such folder gets its
//! own `kargo lsp`, so a workspace with several projects analyzes each with its own dependencies.

import * as vscode from 'vscode';
import { registerActions } from './actions';
import { KargoLenses } from './codeLens';
import { DEBUG_TYPE, KargoDebugAdapters, KargoDebugConfigurations } from './debug';
import { Projects } from './project';
import { reportError } from './report';
import { Selections, StatusBar } from './selection';
import { KargoServers } from './servers';
import { KargoTests } from './testing';
import { KargoTaskProvider, TASK_TYPE } from './tasks';

/** What the extension shares with its integration tests, which read it through the extension's exports. */
export interface KargoApi {
    readonly servers: { running(): number };
    readonly testing: { labels(): string[]; outcomes(): Record<string, string> };
}

export async function activate(context: vscode.ExtensionContext): Promise<KargoApi> {
    const output = vscode.window.createOutputChannel('Kargo');
    const diagnostics = vscode.languages.createDiagnosticCollection('kargo');
    const projects = new Projects(output);
    const selections = new Selections(context.workspaceState);
    const servers = new KargoServers(output);
    const tests = new KargoTests(projects, output);
    const lenses = new KargoLenses(projects, selections);
    const status = new StatusBar(selections);
    const started = new Set<string>();

    /** Starts the server, the watcher and the test discovery of each project folder that is not started yet. */
    const open = async (): Promise<void> => {
        const folders = await Projects.folders();
        status.update(folders.length > 0);
        for (const folder of folders) {
            if (started.has(folder.uri.toString())) continue;
            started.add(folder.uri.toString());
            projects.watch(folder);
            // Not awaited: a language server takes a moment to start, and activation shouldn't wait for it.
            void servers.startFolder(folder);
            void tests.discoverFolder(folder, false);
            void refreshModel(folder);
        }
    };
    /** Reads the model of `folder` again, so its lenses and the status bar follow it. */
    const refreshModel = async (folder: vscode.WorkspaceFolder): Promise<void> => {
        try {
            await projects.metadata(folder);
            lenses.refresh();
            status.update(true);
        } catch (error) {
            reportError(error, output);
        }
    };

    context.subscriptions.push(
        output,
        diagnostics,
        projects,
        selections,
        servers,
        tests,
        lenses,
        status,
        vscode.tasks.registerTaskProvider(TASK_TYPE, new KargoTaskProvider(projects, diagnostics)),
        vscode.debug.registerDebugConfigurationProvider(DEBUG_TYPE, new KargoDebugConfigurations()),
        vscode.debug.registerDebugAdapterDescriptorFactory(DEBUG_TYPE, new KargoDebugAdapters()),
        vscode.languages.registerCodeLensProvider({ language: 'kotlin', scheme: 'file' }, lenses),
        vscode.workspace.onDidSaveTextDocument(document => tests.onSaved(document)),
        vscode.workspace.onDidChangeConfiguration(event => {
            if (event.affectsConfiguration('kargo.path') || event.affectsConfiguration('kargo.lsp')) {
                void servers.restart();
            }
        }),
        vscode.workspace.onDidChangeWorkspaceFolders(event => {
            for (const folder of event.removed) {
                started.delete(folder.uri.toString());
                void servers.stop(folder);
                tests.forget(folder);
                projects.forget(folder);
            }
            void open();
        }),
        projects.onDidChange(folder => {
            void tests.discoverFolder(folder, false);
            void refreshModel(folder);
        }),
    );
    registerActions(context, { output, diagnostics, projects, selections, servers, tests, lenses, status });
    await open();
    return {
        servers: { running: () => servers.running() },
        testing: { labels: () => tests.labels(), outcomes: () => tests.outcomes() },
    };
}

export function deactivate(): void {
    // Everything the extension started is disposed with its subscriptions.
}
