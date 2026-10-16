//! Telling the user that something failed: the message, in the Kargo output and as a notification.

import * as vscode from 'vscode';
import { NOT_INSTALLED } from './executable';

/** The installation guide, which the message about a missing `kargo` links to. */
export const INSTALL_URL = 'https://enekocm7.github.io/kargo/getting-started/installation/';

const GUIDE = 'Installation guide';

/** Shows `error` as an error notification, and writes it to `output`. */
export function reportError(error: unknown, output: vscode.OutputChannel): void {
    const message = error instanceof Error ? error.message : String(error);
    output.appendLine(message);
    const actions = message === NOT_INSTALLED ? [GUIDE] : [];
    void vscode.window.showErrorMessage(message, ...actions).then(choice => {
        if (choice === GUIDE) void vscode.env.openExternal(vscode.Uri.parse(INSTALL_URL));
    });
}
