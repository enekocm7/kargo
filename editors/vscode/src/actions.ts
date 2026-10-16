//! The Kargo commands: the menu of the status bar, the everyday commands as tasks, the pickers for a
//! program, a task, a target and a profile, and adding and removing dependencies.

import * as vscode from 'vscode';
import {
    Profile,
    Selection,
    addArgs,
    buildArgs,
    checkArgs,
    cleanArgs,
    describe,
    fetchArgs,
    removeArgs,
    taskArgs,
    testArgs,
    updateArgs,
} from './commands';
import { DEBUG_COMMAND, KargoLenses, RUN_COMMAND } from './codeLens';
import { debugKargo } from './debug';
import { Hit, DEFAULT_SEARCH_URL, SEARCH_URL_ENV, searchMaven } from './maven';
import { enabledTargets } from './metadata';
import { Projects, kargoBinary } from './project';
import { runProcess } from './process';
import { Selections, StatusBar } from './selection';
import { KargoServers } from './servers';
import { KargoTests } from './testing';
import { kargoTask, runnables } from './tasks';
import { reportError } from './report';

/** What the commands work with. */
export interface Services {
    readonly output: vscode.OutputChannel;
    readonly diagnostics: vscode.DiagnosticCollection;
    readonly projects: Projects;
    readonly selections: Selections;
    readonly servers: KargoServers;
    readonly tests: KargoTests;
    readonly lenses: KargoLenses;
    readonly status: StatusBar;
}

/** Registers the Kargo commands with `context`. */
export function registerActions(context: vscode.ExtensionContext, services: Services): void {
    const register = (id: string, handler: (...args: any[]) => unknown) => {
        context.subscriptions.push(vscode.commands.registerCommand(id, handler));
    };
    const guard = (action: (folder: vscode.WorkspaceFolder) => Thenable<unknown>) => async () => {
        try {
            const folder = await chooseProject();
            if (folder) await action(folder);
        } catch (error) {
            reportError(error, services.output);
        }
    };
    const selected = (): Selection => services.selections.current;

    register('kargo.menu', () => showMenu());
    register(
        'kargo.build',
        guard(folder => runTask(services, folder, buildArgs(selected()))),
    );
    register(
        'kargo.check',
        guard(folder => runTask(services, folder, checkArgs(selected()))),
    );
    register(
        'kargo.test',
        guard(folder => runTask(services, folder, testArgs(undefined, selected()))),
    );
    register(
        'kargo.clean',
        guard(folder => runTask(services, folder, cleanArgs())),
    );
    register(
        'kargo.run',
        guard(folder => pickRunnable(services, folder)),
    );
    register(
        'kargo.runTask',
        guard(folder => pickTask(services, folder)),
    );
    register(
        'kargo.addDependency',
        guard(folder => addDependency(services, folder)),
    );
    register(
        'kargo.removeDependency',
        guard(folder => removeDependency(services, folder)),
    );
    register(
        'kargo.fetch',
        guard(folder => runTask(services, folder, fetchArgs())),
    );
    register(
        'kargo.update',
        guard(folder => runTask(services, folder, updateArgs())),
    );
    register(
        'kargo.selectTarget',
        guard(folder => pickTarget(services, folder)),
    );
    register('kargo.selectProfile', () => pickProfile(services));
    register(
        'kargo.openManifest',
        guard(async folder => {
            await vscode.window.showTextDocument(vscode.Uri.joinPath(folder.uri, 'kargo.toml'));
        }),
    );
    register('kargo.restart', () => restart(services));
    register('kargo.showOutput', () => services.output.show(true));
    // The Run and Debug buttons of the code lenses: their folder, label and `kargo` arguments.
    register(RUN_COMMAND, (folder: string, label: string, args: string[]) => {
        const project = folderOf(folder);
        if (project) return runTask(services, project, args, label);
        return undefined;
    });
    register(DEBUG_COMMAND, (folder: string, label: string, args: string[]) => {
        const project = folderOf(folder);
        if (project) return debugKargo(project, label, args);
        return undefined;
    });
}

/** The project of the active file, else the only project, else the one the user picks. */
async function chooseProject(): Promise<vscode.WorkspaceFolder | undefined> {
    const folders = await Projects.folders();
    if (folders.length === 0) {
        void vscode.window.showWarningMessage('No Kargo project is open: open a folder with a kargo.toml at its root.');
        return undefined;
    }
    const active = vscode.window.activeTextEditor && vscode.workspace.getWorkspaceFolder(vscode.window.activeTextEditor.document.uri);
    const containing = folders.find(folder => active !== undefined && folder.uri.toString() === active.uri.toString());
    if (containing) return containing;
    if (folders.length === 1) return folders[0];
    const picked = await vscode.window.showQuickPick(
        folders.map(folder => ({ label: folder.name, description: folder.uri.fsPath, folder })),
        { placeHolder: 'Which Kargo project?' },
    );
    return picked?.folder;
}

function folderOf(uri: string): vscode.WorkspaceFolder | undefined {
    return vscode.workspace.getWorkspaceFolder(vscode.Uri.parse(uri));
}

/** Runs `kargo <args>` in `folder` as a task, so its output is in the terminal and its errors in the Problems view. */
function runTask(services: Services, folder: vscode.WorkspaceFolder, args: string[], name?: string): Thenable<vscode.TaskExecution> {
    return vscode.tasks.executeTask(kargoTask(folder, args, { name: name ?? describe(args), diagnostics: services.diagnostics }));
}

async function showMenu(): Promise<void> {
    const entries: (vscode.QuickPickItem & { command: string })[] = [
        { label: '$(tools) Build', command: 'kargo.build' },
        { label: '$(check) Check', command: 'kargo.check' },
        { label: '$(beaker) Test', command: 'kargo.test' },
        { label: '$(play) Run a program…', command: 'kargo.run' },
        { label: '$(terminal) Run a task…', command: 'kargo.runTask' },
        { label: '$(trash) Clean', command: 'kargo.clean' },
        { label: '$(add) Add dependency…', command: 'kargo.addDependency' },
        { label: '$(remove) Remove dependency…', command: 'kargo.removeDependency' },
        { label: '$(cloud-download) Fetch dependencies and sources', command: 'kargo.fetch' },
        { label: '$(sync) Update dependencies', command: 'kargo.update' },
        { label: '$(package) Select target…', command: 'kargo.selectTarget' },
        { label: '$(settings) Select profile…', command: 'kargo.selectProfile' },
        { label: '$(file-code) Open kargo.toml', command: 'kargo.openManifest' },
        { label: '$(debug-restart) Restart the Kargo language server', command: 'kargo.restart' },
        { label: '$(output) Show Kargo output', command: 'kargo.showOutput' },
    ];
    const picked = await vscode.window.showQuickPick(entries, { placeHolder: 'Kargo' });
    if (picked) await vscode.commands.executeCommand(picked.command);
}

async function pickRunnable(services: Services, folder: vscode.WorkspaceFolder): Promise<void> {
    const metadata = await services.projects.metadata(folder);
    const items = runnables(metadata, services.selections.current).map(entry => ({ label: entry.label, args: entry.args }));
    if (items.length === 0) {
        void vscode.window.showInformationMessage('This project has no program to run.');
        return;
    }
    const picked = await vscode.window.showQuickPick(items, { placeHolder: 'Run with Kargo' });
    if (picked) await runTask(services, folder, picked.args, picked.label);
}

async function pickTask(services: Services, folder: vscode.WorkspaceFolder): Promise<void> {
    const metadata = await services.projects.metadata(folder);
    const items = metadata.packages.flatMap(pkg =>
        pkg.tasks.map(task => ({ label: task.name, description: task.description, args: taskArgs(task.name) })),
    );
    if (items.length === 0) {
        void vscode.window.showInformationMessage('kargo.toml defines no tasks. Add one under [tasks] to run it here.');
        return;
    }
    const picked = await vscode.window.showQuickPick(items, { placeHolder: 'Run a Kargo task' });
    if (picked) await runTask(services, folder, picked.args, `kargo task ${picked.label}`);
}

async function pickTarget(services: Services, folder: vscode.WorkspaceFolder): Promise<void> {
    const metadata = await services.projects.metadata(folder);
    const items: (vscode.QuickPickItem & { target?: string })[] = [
        { label: 'All targets', description: 'build, run and test each enabled target', target: undefined },
        ...enabledTargets(metadata).map(target => ({ label: target, target })),
    ];
    const picked = await vscode.window.showQuickPick(items, { placeHolder: 'The target for run, build and test' });
    if (picked) {
        await services.selections.setTarget(picked.target);
        services.status.update();
        services.lenses.refresh();
    }
}

async function pickProfile(services: Services): Promise<void> {
    const items: (vscode.QuickPickItem & { profile: Profile })[] = [
        { label: 'dev', description: 'the default profile', profile: 'dev' },
        { label: 'release', description: 'build with --release', profile: 'release' },
    ];
    const picked = await vscode.window.showQuickPick(items, { placeHolder: 'The profile for build, run and test' });
    if (picked) {
        await services.selections.setProfile(picked.profile);
        services.status.update();
    }
}

/** A Maven Central artifact in the Add Dependency pick. */
interface HitItem extends vscode.QuickPickItem {
    readonly coordinates: string;
}

/** Coordinates as `kargo add` takes them: `group:artifact` or `group:artifact:version`. */
const COORDINATES = /^[A-Za-z0-9._-]+:[A-Za-z0-9._-]+(?::[A-Za-z0-9._+-]+)?$/;

/** Searches Maven Central as the user types, and adds the chosen artifact with `kargo add`. */
function addDependency(services: Services, folder: vscode.WorkspaceFolder): Promise<void> {
    return new Promise<void>(resolve => {
        const pick = vscode.window.createQuickPick<HitItem>();
        pick.title = 'Add a dependency';
        pick.placeholder = 'group:artifact, or words to search Maven Central';
        pick.matchOnDescription = true;
        const base = process.env[SEARCH_URL_ENV] || DEFAULT_SEARCH_URL;
        let timer: NodeJS.Timeout | undefined;
        let latest = 0;
        pick.onDidChangeValue(value => {
            clearTimeout(timer);
            timer = setTimeout(async () => {
                const search = ++latest;
                pick.busy = true;
                const hits = await searchMaven(value, base, fetchJson);
                if (search === latest) pick.items = hits.map(hitItem);
                pick.busy = false;
            }, 300);
        });
        pick.onDidAccept(() => {
            // What was typed wins when it is complete: a version, or an artifact the search found. Else the highlighted artifact.
            const typed = pick.value.trim();
            const complete = COORDINATES.test(typed) && (typed.split(':').length === 3 || pick.items.some(item => item.coordinates === typed));
            const chosen = complete ? typed : (pick.selectedItems[0]?.coordinates ?? (COORDINATES.test(typed) ? typed : undefined));
            pick.hide();
            if (chosen === undefined) {
                void vscode.window.showWarningMessage('Type group:artifact, such as io.ktor:ktor-server-core.');
                return;
            }
            void chooseSection().then(async dev => {
                if (dev === undefined) return;
                await runTask(services, folder, addArgs(chosen, dev), `kargo add ${chosen}`);
            });
        });
        pick.onDidHide(() => {
            clearTimeout(timer);
            pick.dispose();
            resolve();
        });
        pick.show();
    });
}

function hitItem(hit: Hit): HitItem {
    return { label: `${hit.group}:${hit.artifact}`, coordinates: `${hit.group}:${hit.artifact}` };
}

/** Whether the dependency goes into `[dev-dependencies]`, or `undefined` when the user cancels. */
async function chooseSection(): Promise<boolean | undefined> {
    const picked = await vscode.window.showQuickPick(
        [
            { label: 'Dependency', description: '[dependencies]', dev: false },
            { label: 'Dev dependency', description: '[dev-dependencies]', dev: true },
        ],
        { placeHolder: 'Add it to' },
    );
    return picked?.dev;
}

async function removeDependency(services: Services, folder: vscode.WorkspaceFolder): Promise<void> {
    const outcome = await runProcess(kargoBinary(), ['info', '--format', 'json'], { cwd: folder.uri.fsPath });
    if (outcome.code !== 0) throw new Error(`\`kargo info\` failed: ${outcome.stderr.trim()}`);
    const info = JSON.parse(outcome.stdout) as {
        dependencies?: { name: string }[];
        'dev-dependencies'?: { name: string }[];
    };
    const items = [
        ...(info.dependencies ?? []).map(dep => ({ label: dep.name, description: '[dependencies]', dev: false })),
        ...(info['dev-dependencies'] ?? []).map(dep => ({ label: dep.name, description: '[dev-dependencies]', dev: true })),
    ];
    if (items.length === 0) {
        void vscode.window.showInformationMessage('kargo.toml has no dependencies to remove.');
        return;
    }
    const picked = await vscode.window.showQuickPick(items, { placeHolder: 'Remove a dependency' });
    if (picked) await runTask(services, folder, removeArgs(picked.label, picked.dev), `kargo remove ${picked.label}`);
}

/** Restarts the language servers, and discovers the tests again. */
async function restart(services: Services): Promise<void> {
    await services.servers.restart();
    services.lenses.refresh();
    await services.tests.discoverAll(true);
}

async function fetchJson(url: string): Promise<unknown> {
    const response = await fetch(url, { headers: { accept: 'application/json' } });
    if (!response.ok) throw new Error(`the search returned HTTP ${response.status}`);
    return response.json();
}
