//! The Kargo projects of the workspace: the `kargo` to run, and each project's model from
//! `kargo metadata --no-deps`, cached until its `kargo.toml` changes.

import * as fs from 'node:fs';
import * as os from 'node:os';
import * as vscode from 'vscode';
import { NOT_INSTALLED, findKargo } from './executable';
import { Metadata, parseMetadata } from './metadata';
import { runProcess } from './process';

/** The `kargo` to run: the `kargo.path` setting, else the one on `PATH`, else `~/.cargo/bin`. Throws when there is none. */
export function kargoBinary(): string {
    const configured = vscode.workspace.getConfiguration('kargo').get<string>('path', '');
    const found = findKargo(configured, { env: process.env, platform: process.platform, home: os.homedir(), isFile });
    if (found === undefined) throw new Error(NOT_INSTALLED);
    return found;
}

function isFile(file: string): boolean {
    try {
        return fs.statSync(file).isFile();
    } catch {
        return false;
    }
}

/** The Kargo projects of the workspace: each folder with a `kargo.toml` at its root. */
export class Projects implements vscode.Disposable {
    private readonly models = new Map<string, Promise<Metadata>>();
    private readonly loaded = new Map<string, Metadata>();
    private readonly watchers: vscode.Disposable[] = [];
    private readonly changes = new vscode.EventEmitter<vscode.WorkspaceFolder>();

    /** Fires when the `kargo.toml` of a folder is created, changed or deleted. */
    readonly onDidChange = this.changes.event;

    constructor(private readonly output: vscode.OutputChannel) {}

    /** The folders of the workspace with a `kargo.toml` at their root. */
    static async folders(): Promise<vscode.WorkspaceFolder[]> {
        const folders = vscode.workspace.workspaceFolders ?? [];
        const found = await Promise.all(folders.map(async folder => ((await isProject(folder)) ? folder : undefined)));
        return found.filter((folder): folder is vscode.WorkspaceFolder => folder !== undefined);
    }

    /** Watches the `kargo.toml` files of `folder`. */
    watch(folder: vscode.WorkspaceFolder): void {
        const watcher = vscode.workspace.createFileSystemWatcher(new vscode.RelativePattern(folder, '**/kargo.toml'));
        const changed = () => {
            this.forget(folder);
            this.changes.fire(folder);
        };
        this.watchers.push(watcher, watcher.onDidChange(changed), watcher.onDidCreate(changed), watcher.onDidDelete(changed));
    }

    /** The model of the project in `folder`, from the cache when its `kargo.toml` hasn't changed. */
    metadata(folder: vscode.WorkspaceFolder): Promise<Metadata> {
        const key = folder.uri.toString();
        let model = this.models.get(key);
        if (model === undefined) {
            model = this.load(folder).then(
                metadata => {
                    this.loaded.set(key, metadata);
                    return metadata;
                },
                error => {
                    this.models.delete(key);
                    throw error;
                },
            );
            this.models.set(key, model);
        }
        return model;
    }

    /** The model of `folder` if it has been loaded already. */
    peek(folder: vscode.WorkspaceFolder): Metadata | undefined {
        return this.loaded.get(folder.uri.toString());
    }

    /** Drops the cached model of `folder`, so the next request runs `kargo metadata` again. */
    forget(folder: vscode.WorkspaceFolder): void {
        this.models.delete(folder.uri.toString());
        this.loaded.delete(folder.uri.toString());
    }

    private async load(folder: vscode.WorkspaceFolder): Promise<Metadata> {
        const program = kargoBinary();
        const outcome = await runProcess(program, ['metadata', '--no-deps'], { cwd: folder.uri.fsPath });
        if (outcome.code !== 0) {
            this.output.appendLine(outcome.stderr.trimEnd());
            throw new Error(`\`kargo metadata\` failed in ${folder.uri.fsPath}: see the Kargo output`);
        }
        return parseMetadata(JSON.parse(outcome.stdout));
    }

    dispose(): void {
        for (const watcher of this.watchers) watcher.dispose();
        this.changes.dispose();
    }
}

/** Whether `folder` has a `kargo.toml` at its root. */
export async function isProject(folder: vscode.WorkspaceFolder): Promise<boolean> {
    try {
        await vscode.workspace.fs.stat(vscode.Uri.joinPath(folder.uri, 'kargo.toml'));
        return true;
    } catch {
        return false;
    }
}
