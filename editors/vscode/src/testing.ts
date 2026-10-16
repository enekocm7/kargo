//! The Test explorer: the tests `kargo test --list` discovers, grouped by project, target and test
//! class, and the runs of `kargo test --message-format json` that set each test's result. Running
//! the failed tests again is the Test explorer's own action: it asks for exactly the items it shows.

import * as vscode from 'vscode';
import { testExplorerArgs, testListArgs } from './commands';
import { Discovered, KargoMessage, TestRef, parseMessage } from './messages';
import { isTestSource, packageOf } from './metadata';
import { Projects, kargoBinary } from './project';
import { runProcess } from './process';

/** A test item's place: a project, a test class of one target, or one test. */
interface Node {
    readonly kind: 'folder' | 'class' | 'test';
    readonly folder: vscode.WorkspaceFolder;
    readonly item: vscode.TestItem;
    /** Classes and tests: the target they are compiled for. */
    readonly target?: string;
    /** Classes and tests: the class, as `kargo test` names it (nested classes use `$`). */
    readonly suite?: string;
    /** Tests: the function name. */
    readonly name?: string;
    /** Tests: Kargo's id of the test within its target. */
    readonly testId?: string;
}

/** One `kargo test` process: a target, narrowed by a filter, and the items it starts. */
interface Plan {
    readonly folder: vscode.WorkspaceFolder;
    readonly target?: string;
    readonly filter?: string;
    /** The items the run starts, and each gets a result or an error. */
    readonly wanted: ReadonlyMap<string, vscode.TestItem>;
    /** Whether every test the run finds is reported, as when the whole project runs. */
    readonly broad: boolean;
}

/** How long to wait after a test source is saved before discovering the tests again. */
const REDISCOVER_DELAY_MS = 1500;

export class KargoTests implements vscode.Disposable {
    private readonly controller = vscode.tests.createTestController('kargo', 'Kargo');
    private readonly nodes = new Map<string, Node>();
    private readonly queues = new Map<string, Promise<void>>();
    private readonly timers = new Map<string, NodeJS.Timeout>();
    /** The outcome of each test in the last run, by its label. */
    private readonly lastOutcomes = new Map<string, string>();

    constructor(
        private readonly projects: Projects,
        private readonly output: vscode.OutputChannel,
    ) {
        this.controller.refreshHandler = () => this.discoverAll(true);
        this.controller.createRunProfile('Run', vscode.TestRunProfileKind.Run, (request, token) => this.run(request, token), true);
    }

    /** Discovers the tests of every project in the workspace. */
    async discoverAll(explicit = false): Promise<void> {
        for (const folder of await Projects.folders()) {
            await this.discoverFolder(folder, explicit);
        }
    }

    /** Discovers the tests of a folder again when one of its test sources is saved. */
    onSaved(document: vscode.TextDocument): void {
        const folder = vscode.workspace.getWorkspaceFolder(document.uri);
        if (folder === undefined || document.languageId !== 'kotlin') return;
        const metadata = this.projects.peek(folder);
        const pkg = metadata && packageOf(metadata, document.uri.fsPath);
        if (!pkg || !isTestSource(pkg, document.uri.fsPath)) return;
        const key = folder.uri.toString();
        clearTimeout(this.timers.get(key));
        this.timers.set(
            key,
            setTimeout(() => {
                this.timers.delete(key);
                void this.discoverFolder(folder, false);
            }, REDISCOVER_DELAY_MS),
        );
    }

    /** The outcome of each test in the last run, by its label, which the integration tests check. */
    outcomes(): Record<string, string> {
        return Object.fromEntries(this.lastOutcomes);
    }

    /** The labels of the classes and tests that are known, which the integration tests check. */
    labels(): string[] {
        return [...this.nodes.values()].filter(node => node.kind !== 'folder').map(node => node.item.label);
    }

    /** Removes the tests of a folder that left the workspace. */
    forget(folder: vscode.WorkspaceFolder): void {
        this.controller.items.delete(idOf('folder', folder.uri.toString()));
        for (const [id, node] of this.nodes) {
            if (node.folder === folder) this.nodes.delete(id);
        }
    }

    dispose(): void {
        for (const timer of this.timers.values()) clearTimeout(timer);
        this.controller.dispose();
    }

    /** Discovers the tests of `folder`, reporting a failure in the output and, when `explicit`, to the user. */
    discoverFolder(folder: vscode.WorkspaceFolder, explicit: boolean): Promise<void> {
        return this.discover(folder).catch(error => {
            const message = `Kargo tests: ${(error as Error).message}`;
            this.output.appendLine(message);
            if (explicit) void vscode.window.showWarningMessage(message);
        });
    }

    /** Lists the tests of `folder`, after the discovery already running for it. */
    private discover(folder: vscode.WorkspaceFolder): Promise<void> {
        const key = folder.uri.toString();
        const next = (this.queues.get(key) ?? Promise.resolve()).then(() => this.listTests(folder));
        this.queues.set(
            key,
            next.catch(() => undefined),
        );
        return next;
    }

    private async listTests(folder: vscode.WorkspaceFolder): Promise<void> {
        const outcome = await runProcess(kargoBinary(), testListArgs(), { cwd: folder.uri.fsPath });
        if (outcome.code !== 0) {
            this.output.appendLine(outcome.stderr.trimEnd());
            throw new Error(`\`kargo test --list\` failed in ${folder.name}: see the Kargo output`);
        }
        const found = outcome.stdout
            .split('\n')
            .map(line => parseMessage(line))
            .flatMap(message => (message?.reason === 'test-discovered' ? [message.test] : []));
        this.replace(folder, found);
    }

    /** Replaces the tests of `folder` with the discovered ones, grouped by target and class. */
    private replace(folder: vscode.WorkspaceFolder, found: readonly Discovered[]): void {
        const root = this.rootOf(folder);
        for (const [id, node] of this.nodes) {
            if (node.folder === folder && node.kind !== 'folder') this.nodes.delete(id);
        }
        root.children.replace([]);
        for (const test of found) this.testOf(folder, test);
    }

    private rootOf(folder: vscode.WorkspaceFolder): vscode.TestItem {
        const id = idOf('folder', folder.uri.toString());
        const existing = this.nodes.get(id);
        if (existing) return existing.item;
        const item = this.controller.createTestItem(id, folder.name, folder.uri);
        this.controller.items.add(item);
        this.nodes.set(id, { kind: 'folder', folder, item });
        return item;
    }

    /** The class a test belongs to, created with the file it is declared in when that is known. */
    private classOf(folder: vscode.WorkspaceFolder, test: TestRef, file?: vscode.Uri): vscode.TestItem {
        const suite = test.suite ?? '';
        const id = idOf('class', folder.uri.toString(), test.target, suite);
        const existing = this.nodes.get(id);
        if (existing) return existing.item;
        const simple = suite.split('.').pop()?.replace(/\$/g, '.') || 'tests';
        const label = test.target && test.target !== 'jvm' ? `${simple} (${test.target})` : simple;
        const item = this.controller.createTestItem(id, label, file);
        this.rootOf(folder).children.add(item);
        this.nodes.set(id, { kind: 'class', folder, item, target: test.target, suite });
        return item;
    }

    private testOf(folder: vscode.WorkspaceFolder, test: Discovered | TestRef): vscode.TestItem {
        const id = idOf('test', folder.uri.toString(), test.target, test.id);
        const existing = this.nodes.get(id);
        if (existing) return existing.item;
        const discovered = test as Discovered;
        const file = discovered.file ? vscode.Uri.file(discovered.file) : undefined;
        const item = this.controller.createTestItem(id, test.displayName ?? test.name ?? test.id, file);
        if (file && discovered.line !== undefined) item.range = new vscode.Range(discovered.line - 1, 0, discovered.line - 1, 0);
        const classItem = this.classOf(folder, test, file);
        classItem.children.add(item);
        this.nodes.set(id, { kind: 'test', folder, item, target: test.target, suite: test.suite, name: test.name, testId: test.id });
        return item;
    }

    /** Runs the requested tests, or every test of the workspace when the request names none. */
    private async run(request: vscode.TestRunRequest, token: vscode.CancellationToken): Promise<void> {
        const run = this.controller.createTestRun(request);
        const plans = await this.plan(request);
        this.lastOutcomes.clear();
        for (const plan of plans) {
            for (const item of plan.wanted.values()) run.enqueued(item);
        }
        const reported = new Set<string>();
        const abort = new AbortController();
        const cancel = token.onCancellationRequested(() => abort.abort());
        try {
            for (const plan of plans) {
                if (abort.signal.aborted) break;
                await this.runPlan(run, plan, abort.signal, reported);
            }
        } catch (error) {
            run.appendOutput(crlf(`${(error as Error).message}\n`));
        } finally {
            cancel.dispose();
        }
        if (!abort.signal.aborted) {
            for (const plan of plans) {
                for (const [id, item] of plan.wanted) {
                    if (!reported.has(id)) {
                        this.lastOutcomes.set(item.label, 'errored');
                        run.errored(item, new vscode.TestMessage('kargo did not report this test: see the output above'));
                    }
                }
            }
        }
        run.end();
    }

    /**
     * The `kargo test` processes that run the requested tests: one per folder and target, narrowed to
     * a single test, or to a test class when all of its tests are requested. A request for no items
     * runs every project whole.
     */
    private async plan(request: vscode.TestRunRequest): Promise<Plan[]> {
        if (request.include === undefined) {
            return (await Projects.folders()).map(folder => ({ folder, broad: true, wanted: this.itemsIn(folder) }));
        }
        const excluded = new Set((request.exclude ?? []).map(item => item.id));
        const groups = new Map<string, { folder: vscode.WorkspaceFolder; target?: string; tests: Map<string, Node> }>();
        for (const item of request.include) {
            const node = this.nodes.get(item.id);
            if (node === undefined) continue;
            for (const test of this.testsOf(node)) {
                if (excluded.has(test.item.id)) continue;
                const key = `${test.folder.uri.toString()}\u0000${test.target ?? ''}`;
                const group = groups.get(key) ?? { folder: test.folder, target: test.target, tests: new Map() };
                group.tests.set(test.item.id, test);
                groups.set(key, group);
            }
        }
        return [...groups.values()].map(group => {
            const tests = [...group.tests.values()];
            const suites = new Set(tests.map(test => test.suite ?? ''));
            let filter: string | undefined;
            if (tests.length === 1) filter = filterOf(tests[0]);
            else if (suites.size === 1 && tests[0].suite) filter = tests[0].suite;
            const wanted = new Map(tests.map(test => [test.item.id, test.item] as const));
            return { folder: group.folder, target: group.target, filter, wanted, broad: false };
        });
    }

    /** The tests of a node: the test itself, or every test of a class or a project. */
    private testsOf(node: Node): Node[] {
        const tests = [...this.nodes.values()].filter(other => other.kind === 'test' && other.folder === node.folder);
        if (node.kind === 'folder') return tests;
        if (node.kind === 'test') return [node];
        return tests.filter(test => test.target === node.target && test.suite === node.suite);
    }

    private itemsIn(folder: vscode.WorkspaceFolder): Map<string, vscode.TestItem> {
        return new Map(
            [...this.nodes.values()].filter(node => node.kind === 'test' && node.folder === folder).map(node => [node.item.id, node.item] as const),
        );
    }

    private async runPlan(run: vscode.TestRun, plan: Plan, signal: AbortSignal, reported: Set<string>): Promise<void> {
        const outcome = await runProcess(kargoBinary(), testExplorerArgs(plan.filter, plan.target), {
            cwd: plan.folder.uri.fsPath,
            signal,
            onStdoutLine: line => this.onLine(run, plan, reported, line),
            onStderrLine: line => run.appendOutput(crlf(`${line}\n`)),
        });
        if (outcome.code !== 0 && outcome.code !== null) {
            run.appendOutput(`kargo test exited with code ${outcome.code}\r\n`);
        }
    }

    private onLine(run: vscode.TestRun, plan: Plan, reported: Set<string>, line: string): void {
        const message = parseMessage(line);
        if (message === undefined) {
            run.appendOutput(crlf(`${line}\n`));
            return;
        }
        this.report(run, plan, reported, message);
    }

    private report(run: vscode.TestRun, plan: Plan, reported: Set<string>, message: KargoMessage): void {
        switch (message.reason) {
            case 'test-started': {
                const item = this.itemFor(plan, message.test);
                if (item) run.started(item);
                return;
            }
            case 'test-finished': {
                const item = this.itemFor(plan, message.test);
                if (item === undefined) return;
                reported.add(item.id);
                this.lastOutcomes.set(item.label, message.outcome);
                if (message.stdout) run.appendOutput(crlf(message.stdout), undefined, item);
                if (message.stderr) run.appendOutput(crlf(message.stderr), undefined, item);
                if (message.outcome === 'passed') {
                    run.passed(item, message.durationMs);
                } else if (message.outcome === 'ignored') {
                    run.skipped(item);
                } else {
                    const headline = message.message ?? 'failed';
                    const failure = new vscode.TestMessage(message.details ? `${headline}\n${message.details}` : headline);
                    if (item.uri && item.range) failure.location = new vscode.Location(item.uri, item.range);
                    run.failed(item, failure, message.durationMs);
                }
                return;
            }
            case 'test-summary':
                run.appendOutput(`${message.target || 'tests'}: ${message.passed} passed, ${message.failed} failed, ${message.ignored} ignored\r\n`);
                return;
            case 'compiler-message':
                run.appendOutput(crlf(`${message.rendered}\n`));
                return;
            default:
                return;
        }
    }

    /** The item a test's result belongs to, or `undefined` when this run doesn't report that test. */
    private itemFor(plan: Plan, test: TestRef): vscode.TestItem | undefined {
        const id = idOf('test', plan.folder.uri.toString(), test.target, test.id);
        if (!plan.broad && !plan.wanted.has(id)) return undefined;
        const existing = this.nodes.get(id);
        if (existing) return existing.item;
        return this.testOf(plan.folder, test);
    }
}

/** The filter that runs one test: `Class.function`, the name `kargo test` matches as a substring. */
function filterOf(test: Node): string {
    return test.name && test.suite ? `${test.suite}.${test.name}` : (test.suite ?? test.name ?? '');
}

/** A stable id for a test item, from the fields that identify it. */
function idOf(...parts: (string | undefined)[]): string {
    return JSON.stringify(parts);
}

function crlf(text: string): string {
    return text.replace(/\r?\n/g, '\r\n');
}
