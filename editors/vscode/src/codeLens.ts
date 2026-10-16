//! Run and Debug buttons above `fun main`, the test functions and the test classes of a Kargo
//! package. The commands behind them run the same `kargo` commands as the tasks do.

import * as path from 'node:path';
import * as vscode from 'vscode';
import { RunTarget, Selection, runArgs, testArgs } from './commands';
import { scanKotlin } from './kotlinScan';
import { binOf, exampleOf, isTestSource, packageOf, targetOf, Package } from './metadata';
import { Projects } from './project';
import { Selections } from './selection';

/** The command that runs a lens's `kargo` arguments as a task. Its arguments: folder URI, label, arguments. */
export const RUN_COMMAND = 'kargo.lens.run';
/** The command that debugs a lens's `kargo` arguments. Its arguments: folder URI, label, arguments. */
export const DEBUG_COMMAND = 'kargo.lens.debug';

export class KargoLenses implements vscode.CodeLensProvider, vscode.Disposable {
    private readonly changes = new vscode.EventEmitter<void>();

    /** Fires when the lenses should be asked for again, such as when the target changes. */
    readonly onDidChangeCodeLenses = this.changes.event;

    constructor(
        private readonly projects: Projects,
        private readonly selections: Selections,
    ) {}

    refresh(): void {
        this.changes.fire();
    }

    provideCodeLenses(document: vscode.TextDocument): vscode.CodeLens[] {
        if (document.uri.scheme !== 'file' || document.languageId !== 'kotlin') return [];
        const folder = vscode.workspace.getWorkspaceFolder(document.uri);
        const metadata = folder && this.projects.peek(folder);
        const file = document.uri.fsPath;
        const pkg = metadata && packageOf(metadata, file);
        if (!folder || !metadata || !pkg) return [];

        const selection = this.selections.current;
        const scan = scanKotlin(document.getText());
        const source = targetOf(pkg, file);
        const packaged = metadata.packages.filter(p => p.member).length > 1 ? pkg.name : undefined;
        const folderUri = folder.uri.toString();
        const lenses: vscode.CodeLens[] = [];
        const lens = (line: number, label: string, args: string[], debuggable: boolean) =>
            lenses.push(...lensPair(new vscode.Range(line, 0, line, 0), folderUri, label, args, debuggable));

        // The program of this file, or of the `[[bin]]` or example it is.
        const program = programOf(pkg, file, source ?? selection.target, selection);
        for (const line of scan.mains) {
            lens(line, program.label, runArgs(program.run, selection, packaged), program.debuggable);
        }

        if (isTestSource(pkg, file)) {
            const testTarget = source ?? selection.target;
            const debuggable = isJvm(testTarget);
            const testSelection = { ...selection, target: testTarget };
            const testClasses = new Set(scan.tests.map(test => test.className).filter((name): name is string => name !== undefined));
            for (const cls of scan.classes) {
                if (!testClasses.has(cls.path)) continue;
                const filter = qualified(scan.packageName, cls.path);
                lens(cls.line, `tests in ${filter}`, testArgs(filter, testSelection, packaged), debuggable);
            }
            for (const test of scan.tests) {
                // A top-level test runs in the class its file compiles to.
                const filter = qualified(scan.packageName, test.className ?? facadeOf(file), test.name);
                lens(test.line, `test ${filter}`, testArgs(filter, testSelection, packaged), debuggable);
            }
        }
        return lenses;
    }

    dispose(): void {
        this.changes.dispose();
    }
}

/** What a `fun main` runs: the `[[bin]]` or example of the file, else the program of its target. */
function programOf(
    pkg: Package,
    file: string,
    target: string | undefined,
    selection: Selection,
): { run: RunTarget; label: string; debuggable: boolean } {
    const debuggable = isJvm(target ?? selection.target);
    const bin = binOf(pkg, file);
    if (bin) return { run: { kind: 'bin', name: bin.name }, label: `run ${bin.name}`, debuggable };
    const example = exampleOf(pkg, file);
    if (example) return { run: { kind: 'example', name: example.name }, label: `run example ${example.name}`, debuggable };
    return { run: { kind: 'program', target }, label: `run ${path.basename(file)}`, debuggable };
}

/** Run and Debug, on one line. Debugging is for the JVM, and an unnamed target is the JVM's by default. */
function lensPair(range: vscode.Range, folder: string, label: string, args: string[], debuggable: boolean): vscode.CodeLens[] {
    const command = (title: string, id: string, tooltip: string) =>
        new vscode.CodeLens(range, { title, tooltip: `${tooltip} ${label} with Kargo`, command: id, arguments: [folder, label, args] });
    const lenses = [command('$(play) Run', RUN_COMMAND, 'Run')];
    if (debuggable) lenses.push(command('$(debug-alt) Debug', DEBUG_COMMAND, 'Debug'));
    return lenses;
}

function isJvm(target: string | undefined): boolean {
    return target === undefined || target === 'jvm';
}

/** `package.Class.name`, leaving out the parts that are not there. */
function qualified(packageName: string | undefined, ...parts: (string | undefined)[]): string {
    return [packageName, ...parts].filter((part): part is string => part !== undefined && part !== '').join('.');
}

/** The class Kotlin compiles a file's top-level functions into: its name with `Kt` at the end. */
function facadeOf(file: string): string {
    const base = path.basename(file, path.extname(file));
    return `${base.charAt(0).toUpperCase()}${base.slice(1)}Kt`;
}
