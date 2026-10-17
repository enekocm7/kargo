//! `kargo lsp` for each project folder: Kotlin and Java files, and the `kargo.toml` files of the
//! folder. One server per folder, so a workspace with several projects analyzes each with its own
//! dependencies.

import * as path from 'node:path';
import * as vscode from 'vscode';
import { LanguageClient, LanguageClientOptions, ServerOptions, State } from 'vscode-languageclient/node';
import { Projects, kargoBinary } from './project';
import { reportError } from './report';

/** The `kargo.lsp.*` settings, as the server needs them. */
export interface ServerSettings {
    readonly server: string;
    readonly checkOnSave: boolean;
    readonly checkTargets: readonly string[];
    readonly fetchSources: boolean;
}

export function serverSettings(scope: vscode.ConfigurationScope): ServerSettings {
    const config = vscode.workspace.getConfiguration('kargo', scope);
    return {
        server: config.get<string>('lsp.server', 'kargo'),
        checkOnSave: config.get<boolean>('lsp.checkOnSave', true),
        checkTargets: config.get<string[]>('lsp.checkTargets', []),
        fetchSources: config.get<boolean>('lsp.fetchSources', true),
    };
}

/** The `initializationOptions` of `kargo lsp`: only the settings that differ from the server's defaults. */
export function initializationOptions(settings: ServerSettings): Record<string, unknown> {
    const options: Record<string, unknown> = { checkOnSave: settings.checkOnSave };
    if (settings.checkTargets.length > 0) options.checkTargets = [...settings.checkTargets];
    if (!settings.fetchSources) options.fetchSources = false;
    return options;
}

/** The arguments of `kargo lsp` for `settings`. */
export function serverArguments(settings: ServerSettings): string[] {
    return settings.server === 'kargo' ? ['lsp'] : ['lsp', '--server', settings.server];
}

/** Whether `uri` is a `kargo.toml`, the one file name Kargo's server treats as a manifest. */
function isManifest(uri: vscode.Uri): boolean {
    return path.basename(uri.fsPath) === 'kargo.toml';
}

export class KargoServers implements vscode.Disposable {
    private readonly clients = new Map<string, LanguageClient>();
    private readonly log = vscode.window.createOutputChannel('Kargo Language Server', { log: true });

    /** `output` is the Kargo output channel, where a server that can't start is reported. */
    constructor(private readonly output: vscode.OutputChannel) {}

    /** Starts the server of `folder`, reporting a failure to the user instead of throwing. */
    async startFolder(folder: vscode.WorkspaceFolder): Promise<void> {
        try {
            await this.start(folder);
        } catch (error) {
            reportError(error, this.output);
        }
    }

    /** Stops every server and starts one for each project again, with the settings as they are now. */
    async restart(): Promise<void> {
        await this.stopAll();
        for (const folder of await Projects.folders()) {
            await this.startFolder(folder);
        }
    }

    /** Starts `kargo lsp` for `folder`, unless it runs already. */
    async start(folder: vscode.WorkspaceFolder): Promise<void> {
        const key = folder.uri.toString();
        if (this.clients.has(key)) return;
        const settings = serverSettings(folder.uri);
        const serverOptions: ServerOptions = {
            command: kargoBinary(),
            args: serverArguments(settings),
            options: { cwd: folder.uri.fsPath },
        };
        const inFolder = (pattern: string) => ({ baseUri: key, pattern });
        const clientOptions: LanguageClientOptions = {
            documentSelector: [
                { scheme: 'file', language: 'kotlin', pattern: inFolder('**/*.{kt,kts}') },
                { scheme: 'file', language: 'java', pattern: inFolder('**/*.java') },
                { scheme: 'file', pattern: inFolder('**/kargo.toml') },
            ],
            initializationOptions: initializationOptions(settings),
            workspaceFolder: folder,
            outputChannel: this.log,
            diagnosticCollectionName: 'kargo-lsp',
            middleware: {
                // Without a Kotlin server (`kargo.lsp.server` is `none`), `kargo lsp` answers `textDocument/codeAction`
                // for Kotlin and Java files with "needs a Kotlin language server", which VS Code shows as an error.
                provideCodeActions: (document, range, context, token, next) =>
                    settings.server !== 'none' || isManifest(document.uri) ? next(document, range, context, token) : undefined,
            },
        };
        const client = new LanguageClient('kargo', `Kargo (${folder.name})`, serverOptions, clientOptions);
        this.clients.set(key, client);
        try {
            await client.start();
        } catch (error) {
            this.clients.delete(key);
            throw error;
        }
    }

    /** Stops the server of `folder`, if it runs. */
    async stop(folder: vscode.WorkspaceFolder): Promise<void> {
        const key = folder.uri.toString();
        const client = this.clients.get(key);
        this.clients.delete(key);
        await client?.stop();
    }

    /** The number of servers that are running. */
    running(): number {
        return [...this.clients.values()].filter(client => client.state === State.Running).length;
    }

    /** Stops every server, so the settings are read again when they start. */
    async stopAll(): Promise<void> {
        const clients = [...this.clients.values()];
        this.clients.clear();
        await Promise.all(clients.map(client => client.stop()));
    }

    dispose(): void {
        void this.stopAll();
        this.log.dispose();
    }
}
