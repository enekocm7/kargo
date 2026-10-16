//! Runs the integration tests: starts VS Code with this extension and a new Kargo project, and runs
//! the suite inside it. Needs `kargo` on the PATH and the network for VS Code's download the first
//! time. `VSCODE_VERSION` picks the VS Code version (default: the oldest the extension supports).

import { execFileSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runTests } from '@vscode/test-electron';

async function main(): Promise<void> {
    // out/integration/runTests.js: the extension is two directories up.
    const extensionRoot = path.resolve(__dirname, '..', '..');
    const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'kargo-vscode-'));
    const project = path.join(workspace, 'app');
    execFileSync('kargo', ['new', project, '--target', 'jvm', '--no-vcs'], { stdio: 'inherit' });
    try {
        await runTests({
            version: process.env.VSCODE_VERSION ?? '1.100.0',
            extensionDevelopmentPath: extensionRoot,
            extensionTestsPath: path.join(__dirname, 'suite.js'),
            // Built-in extensions stay on: VS Code's debug support is one of them, and --disable-extensions
            // takes it away, so a debug session would end the window.
            launchArgs: [project],
        });
    } finally {
        fs.rmSync(workspace, { recursive: true, force: true });
    }
}

main().catch(error => {
    console.error(error);
    process.exitCode = 1;
});
