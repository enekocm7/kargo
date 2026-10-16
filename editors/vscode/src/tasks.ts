//! Kargo's tasks: `kargo build`, `run`, `test` and the rest, in the terminal, with their compiler
//! messages in the Problems view under the `kargo` collection. A task runs `kargo` itself, so the
//! output is read here rather than by a VS Code problem matcher: an extension can't register one,
//! and the file paths in the output are resolved against the folder the command runs in.

import { ChildProcess, spawn } from 'node:child_process';
import * as path from 'node:path';
import * as vscode from 'vscode';
import { Selection, buildArgs, checkArgs, cleanArgs, describe, fetchArgs, runArgs, taskArgs, testArgs, updateArgs } from './commands';
import { parseCompilerLine } from './diagnostics';
import { Metadata, Package, packageTargets } from './metadata';
import { kargoBinary, Projects } from './project';
import { LineReader } from './process';

/** The task type of Kargo's tasks in `tasks.json`: `{ "type": "kargo", "args": ["build"] }`. */
export const TASK_TYPE = 'kargo';

/** What a `kargo` task runs: the arguments after `kargo`, and the folder to run it in. */
export interface KargoTaskDefinition extends vscode.TaskDefinition {
    readonly args: string[];
    /** Relative to the workspace folder; default: the folder. */
    readonly cwd?: string;
}

/** The `kargo` tasks of every project in the workspace, and the ones a user writes in `tasks.json`. */
export class KargoTaskProvider implements vscode.TaskProvider {
    constructor(
        private readonly projects: Projects,
        private readonly diagnostics: vscode.DiagnosticCollection,
    ) {}

    async provideTasks(): Promise<vscode.Task[]> {
        const tasks: vscode.Task[] = [];
        for (const folder of await Projects.folders()) {
            let metadata: Metadata;
            try {
                metadata = await this.projects.metadata(folder);
            } catch {
                continue;
            }
            tasks.push(...projectTasks(folder, metadata, this.diagnostics));
        }
        return tasks;
    }

    resolveTask(task: vscode.Task): vscode.Task | undefined {
        const definition = task.definition as KargoTaskDefinition;
        const folder = task.scope !== undefined && typeof task.scope === 'object' && 'uri' in task.scope ? task.scope : undefined;
        if (folder === undefined || !Array.isArray(definition.args)) return undefined;
        return kargoTask(folder, definition.args, { name: task.name, group: task.group, cwd: definition.cwd, diagnostics: this.diagnostics });
    }
}

/** A task running `kargo <args>` in `folder`. */
export function kargoTask(
    folder: vscode.WorkspaceFolder,
    args: readonly string[],
    options: { name?: string; group?: vscode.TaskGroup; cwd?: string; diagnostics: vscode.DiagnosticCollection },
): vscode.Task {
    const definition: KargoTaskDefinition = { type: TASK_TYPE, args: [...args], cwd: options.cwd };
    const execution = new vscode.CustomExecution(async (resolved: vscode.TaskDefinition) => {
        const { args: taskArguments, cwd } = resolved as KargoTaskDefinition;
        const directory = cwd ? path.resolve(folder.uri.fsPath, cwd) : folder.uri.fsPath;
        return new KargoTerminal(taskArguments, directory, options.diagnostics);
    });
    const task = new vscode.Task(definition, folder, options.name ?? describe(args), 'Kargo', execution);
    task.group = options.group;
    return task;
}

/**
 * The tasks of a project: the everyday commands, then a run and a test per target, each program,
 * `[[bin]]`, example and task of `kargo.toml`. A workspace's commands name their package.
 */
export function projectTasks(folder: vscode.WorkspaceFolder, metadata: Metadata, diagnostics: vscode.DiagnosticCollection): vscode.Task[] {
    const seen = new Set<string>();
    const tasks: vscode.Task[] = [];
    const add = (args: string[], group?: vscode.TaskGroup) => {
        const key = describe(args);
        if (seen.has(key)) return;
        seen.add(key);
        tasks.push(kargoTask(folder, args, { group, diagnostics }));
    };
    add(buildArgs(), vscode.TaskGroup.Build);
    add(buildArgs({ profile: 'release' }), vscode.TaskGroup.Build);
    add(checkArgs());
    add(testArgs(undefined), vscode.TaskGroup.Test);
    add(cleanArgs(), vscode.TaskGroup.Clean);
    add(fetchArgs());
    add(updateArgs());
    const members = metadata.packages.filter(pkg => pkg.member);
    for (const pkg of members) {
        const packageName = members.length > 1 ? pkg.name : undefined;
        for (const kind of packageTargets(pkg)) {
            add(runArgs({ kind: 'program', target: kind }, {}, packageName));
            add(testArgs(undefined, { target: kind }, packageName));
        }
        for (const bin of pkg.bins) add(runArgs({ kind: 'bin', name: bin.name }, {}, packageName));
        for (const example of pkg.examples) add(runArgs({ kind: 'example', name: example.name }, {}, packageName));
        for (const task of pkg.tasks) add(taskArgs(task.name));
    }
    return tasks;
}

/** The programs and tasks of a project, as the Run and Run Task pickers list them. */
export function runnables(metadata: Metadata, selection: Selection = {}): { label: string; args: string[] }[] {
    const members = metadata.packages.filter(pkg => pkg.member);
    return members.flatMap((pkg: Package) => {
        const packageName = members.length > 1 ? pkg.name : undefined;
        const programs = packageTargets(pkg).map(kind => ({
            label: `Run ${kind}${packageName ? ` (${pkg.name})` : ''}`,
            args: runArgs({ kind: 'program', target: kind }, selection, packageName),
        }));
        const bins = pkg.bins.map(bin => ({ label: `Run bin ${bin.name}`, args: runArgs({ kind: 'bin', name: bin.name }, selection, packageName) }));
        const examples = pkg.examples.map(example => ({
            label: `Run example ${example.name}`,
            args: runArgs({ kind: 'example', name: example.name }, selection, packageName),
        }));
        return [...programs, ...bins, ...examples];
    });
}

/**
 * The terminal of a Kargo task: the output of `kargo <args>`, with its compiler messages in the
 * Problems view. Its stdin is the program's, so a program that reads input works.
 */
class KargoTerminal implements vscode.Pseudoterminal {
    private readonly writes = new vscode.EventEmitter<string>();
    private readonly closes = new vscode.EventEmitter<number>();
    private readonly files = new Map<string, vscode.Diagnostic[]>();
    private readonly stdout = new LineReader(line => this.compilerLine(line));
    private readonly stderr = new LineReader(line => this.compilerLine(line));
    private child: ChildProcess | undefined;

    readonly onDidWrite = this.writes.event;
    readonly onDidClose = this.closes.event;

    constructor(
        private readonly args: readonly string[],
        private readonly cwd: string,
        private readonly diagnostics: vscode.DiagnosticCollection,
    ) {}

    open(): void {
        this.diagnostics.clear();
        this.files.clear();
        let program: string;
        try {
            program = kargoBinary();
        } catch (error) {
            this.write(`${(error as Error).message}\n`);
            this.closes.fire(1);
            return;
        }
        this.write(`> ${describe(this.args)}\n`);
        const child = spawn(program, [...this.args], { cwd: this.cwd, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
        this.child = child;
        child.stdout.setEncoding('utf8');
        child.stderr.setEncoding('utf8');
        child.stdout.on('data', (chunk: string) => {
            this.write(chunk);
            this.stdout.push(chunk);
        });
        child.stderr.on('data', (chunk: string) => {
            this.write(chunk);
            this.stderr.push(chunk);
        });
        child.on('error', error => {
            this.write(`could not start \`${program}\`: ${error.message}\n`);
            this.closes.fire(1);
        });
        child.on('close', code => {
            this.stdout.end();
            this.stderr.end();
            this.child = undefined;
            this.closes.fire(code ?? 1);
        });
    }

    close(): void {
        this.child?.kill();
        this.child = undefined;
    }

    handleInput(data: string): void {
        this.child?.stdin?.write(data);
    }

    private compilerLine(line: string): void {
        const message = parseCompilerLine(line);
        if (message === undefined) return;
        const file = vscode.Uri.file(path.isAbsolute(message.file) ? message.file : path.resolve(this.cwd, message.file));
        const first = message.line - 1;
        const column = Math.max((message.column ?? 1) - 1, 0);
        const diagnostic = new vscode.Diagnostic(
            new vscode.Range(first, column, first, column + 1),
            message.message,
            severityOf(message.severity),
        );
        diagnostic.source = 'kargo';
        const list = this.files.get(file.toString()) ?? [];
        list.push(diagnostic);
        this.files.set(file.toString(), list);
        this.diagnostics.set(file, list);
    }

    private write(text: string): void {
        this.writes.fire(text.replace(/\r?\n/g, '\r\n'));
    }
}

function severityOf(severity: 'error' | 'warning' | 'info'): vscode.DiagnosticSeverity {
    switch (severity) {
        case 'error':
            return vscode.DiagnosticSeverity.Error;
        case 'warning':
            return vscode.DiagnosticSeverity.Warning;
        default:
            return vscode.DiagnosticSeverity.Information;
    }
}
