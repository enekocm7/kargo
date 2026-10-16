//! The target and profile the status bar selects, which the commands, code lenses and tasks use. They
//! are kept per workspace, so they survive a reload.

import * as vscode from 'vscode';
import { Profile, Selection } from './commands';

const TARGET_KEY = 'kargo.target';
const PROFILE_KEY = 'kargo.profile';

export class Selections implements vscode.Disposable {
    private readonly changes = new vscode.EventEmitter<Selection>();

    /** Fires when the target or the profile changes. */
    readonly onDidChange = this.changes.event;

    constructor(private readonly state: vscode.Memento) {}

    /** The current target and profile. `target` is `undefined` for every enabled target. */
    get current(): Selection {
        const target = this.state.get<string>(TARGET_KEY);
        const profile = this.state.get<Profile>(PROFILE_KEY);
        return { target, profile: profile === 'release' ? 'release' : 'dev' };
    }

    async setTarget(target: string | undefined): Promise<void> {
        await this.state.update(TARGET_KEY, target);
        this.changes.fire(this.current);
    }

    async setProfile(profile: Profile): Promise<void> {
        await this.state.update(PROFILE_KEY, profile);
        this.changes.fire(this.current);
    }

    dispose(): void {
        this.changes.dispose();
    }
}

/** The status bar item showing the selection; clicking it opens the Kargo menu. */
export class StatusBar implements vscode.Disposable {
    private readonly item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);

    constructor(private readonly selections: Selections) {
        this.item.command = 'kargo.menu';
        this.item.name = 'Kargo';
    }

    /** Shows the selection, or hides the item when the workspace has no Kargo project. */
    update(visible = true): void {
        if (!visible) {
            this.item.hide();
            return;
        }
        const { target, profile } = this.selections.current;
        this.item.text = `$(package) Kargo: ${target ?? 'all targets'} · ${profile}`;
        this.item.tooltip = 'Kargo: the target and profile for run, build and test. Click for the Kargo menu.';
        this.item.show();
    }

    dispose(): void {
        this.item.dispose();
    }
}
