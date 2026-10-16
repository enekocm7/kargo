//! The integration suite, run inside VS Code by `runTests.ts` on a new Kargo project: the extension
//! starts, lists the tasks, starts its language server, discovers the tests, builds the project,
//! reports a compiler error in the Problems view and offers Run and Debug above `fun main`.

import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as vscode from 'vscode';
import type { KargoApi } from '../extension';

const EXTENSION_ID = 'enekocm7.kargo';
/** Compiling the project for the first time downloads the Kotlin compiler, so the waits are long. */
const SLOW_MS = 240_000;

export async function run(): Promise<void> {
    const folder = vscode.workspace.workspaceFolders?.[0];
    assert.ok(folder, 'the test workspace is open');
    const extension = vscode.extensions.getExtension<KargoApi>(EXTENSION_ID);
    assert.ok(extension, `${EXTENSION_ID} is installed`);
    const api = await extension.activate();

    const checks: [string, () => Promise<void>][] = [
        ['registers its commands', async () => {
            const commands = await vscode.commands.getCommands(true);
            for (const command of ['kargo.build', 'kargo.test', 'kargo.menu', 'kargo.selectTarget', 'kargo.addDependency']) {
                assert.ok(commands.includes(command), `${command} is registered`);
            }
        }],
        ['offers the tasks of kargo.toml', async () => {
            const names = (await vscode.tasks.fetchTasks({ type: 'kargo' })).map(task => task.name);
            for (const name of ['kargo build', 'kargo test', 'kargo clean', 'kargo fetch --sources']) {
                assert.ok(names.includes(name), `${name} is offered among ${names.join(', ')}`);
            }
        }],
        ['starts kargo lsp for the project', () => waitFor(() => api.servers.running() === 1, SLOW_MS, 'kargo lsp is running')],
        ['discovers the tests', () => waitFor(() => api.testing.labels().includes('GreetingTest'), SLOW_MS, 'the tests are discovered')],
        ['builds the project', async () => {
            const code = await runTask('kargo build');
            assert.equal(code, 0, 'kargo build exits with 0');
        }],
        ['starts a debug session through kargo dap', async () => {
            const started = await vscode.debug.startDebugging(folder, {
                type: 'kargo',
                request: 'launch',
                name: 'Kargo: run the program',
                args: ['run'],
            });
            assert.equal(started, true, 'the session starts');
            await waitFor(() => vscode.debug.activeDebugSession?.type === 'kargo', SLOW_MS, 'the Kargo session is active');
            await vscode.debug.stopDebugging();
        }],
        ['lists the compiler errors in the Problems view', async () => {
            const broken = path.join(folder.uri.fsPath, 'src', 'Broken.kt');
            fs.writeFileSync(broken, 'package com.example.app\n\nval answer: Int = "not a number"\n');
            try {
                const code = await runTask('kargo build');
                assert.notEqual(code, 0, 'a build with an error fails');
                const diagnostics = vscode.languages.getDiagnostics(vscode.Uri.file(broken)).filter(d => d.source === 'kargo');
                assert.ok(diagnostics.length > 0, 'the error is in the Problems view');
                assert.equal(diagnostics[0].range.start.line, 2, 'the error is on line 3');
                assert.equal(diagnostics[0].severity, vscode.DiagnosticSeverity.Error);
            } finally {
                fs.rmSync(broken, { force: true });
            }
        }],
        ['puts Run and Debug above fun main', async () => {
            const main = vscode.Uri.file(path.join(folder.uri.fsPath, 'src', 'Main.kt'));
            await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(main));
            const titles = await waitForLenses(main, lens => lens.command?.command === 'kargo.lens.run');
            assert.ok(titles.includes('$(play) Run'), `Run is offered among ${titles.join(', ')}`);
            assert.ok(titles.includes('$(debug-alt) Debug'), 'Debug is offered for the JVM target');
        }],
        ['reports the results of a run in the test explorer', async () => {
            const failing = path.join(folder.uri.fsPath, 'tests', 'FailingTest.kt');
            fs.writeFileSync(
                failing,
                [
                    'package com.example.app',
                    '',
                    'import kotlin.test.Test',
                    'import kotlin.test.assertEquals',
                    '',
                    'class FailingTest {',
                    '    @Test',
                    '    fun alwaysFails() {',
                    '        assertEquals(1, 2)',
                    '    }',
                    '}',
                    '',
                ].join('\n'),
            );
            try {
                await vscode.commands.executeCommand('testing.refreshTests');
                await waitFor(() => api.testing.labels().includes('FailingTest'), SLOW_MS, 'the new test is discovered');
                await vscode.commands.executeCommand('testing.runAll');
                await waitFor(() => Object.keys(api.testing.outcomes()).length >= 2, SLOW_MS, 'the tests report their results');
                const outcomes = api.testing.outcomes();
                assert.equal(outcomes['greetsByName()'], 'passed', 'the passing test passes');
                assert.equal(outcomes['alwaysFails()'], 'failed', 'the failing test fails');
            } finally {
                fs.rmSync(failing, { force: true });
            }
        }],
        ['puts Run above a test', async () => {
            const test = vscode.Uri.file(path.join(folder.uri.fsPath, 'tests', 'GreetingTest.kt'));
            await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(test));
            const titles = await waitForLenses(test, lens => {
                const args = lens.command?.arguments?.[2] as string[] | undefined;
                return args?.some(arg => arg.endsWith('GreetingTest.greetsByName')) === true;
            });
            assert.ok(titles.includes('$(play) Run'), 'Run is offered for the test');
        }],
    ];

    const failures: string[] = [];
    for (const [name, check] of checks) {
        try {
            await check();
            console.log(`ok - ${name}`);
        } catch (error) {
            failures.push(name);
            console.error(`not ok - ${name}: ${(error as Error).message}`);
        }
    }
    assert.deepEqual(failures, [], 'every integration check passes');
}

/** Runs the task called `name`, and resolves with its exit code once it has ended. */
async function runTask(name: string): Promise<number | undefined> {
    const task = (await vscode.tasks.fetchTasks({ type: 'kargo' })).find(candidate => candidate.name === name);
    if (!task) throw new Error(`no task named ${name}`);
    const ended = new Promise<number | undefined>(resolve => {
        const listener = vscode.tasks.onDidEndTaskProcess(event => {
            if (event.execution.task.name !== name) return;
            listener.dispose();
            resolve(event.exitCode);
        });
    });
    await vscode.tasks.executeTask(task);
    return ended;
}

/** The titles of the code lenses of `uri` once one of them satisfies `matches`. */
async function waitForLenses(uri: vscode.Uri, matches: (lens: vscode.CodeLens) => boolean): Promise<string[]> {
    let titles: string[] = [];
    await waitFor(async () => {
        const lenses = (await vscode.commands.executeCommand<vscode.CodeLens[]>('vscode.executeCodeLensProvider', uri)) ?? [];
        if (!lenses.some(matches)) return false;
        titles = lenses.map(lens => lens.command?.title ?? '');
        return true;
    }, SLOW_MS, 'the code lenses appear');
    return titles;
}

/** Polls `condition` until it holds, failing with `what` after `timeoutMs`. */
async function waitFor(condition: () => boolean | Promise<boolean>, timeoutMs: number, what: string): Promise<void> {
    const deadline = Date.now() + timeoutMs;
    while (!(await condition())) {
        if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
        await new Promise(resolve => setTimeout(resolve, 500));
    }
}
