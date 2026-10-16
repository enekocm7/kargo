//! The `kargo` command lines the extension runs. Each function returns the arguments after the
//! `kargo` program, so tasks, code lenses and the test explorer build the same commands.

/** `dev` or `release`: the profile `--release` selects. */
export type Profile = 'dev' | 'release';

/** What the status bar picked: a target (`undefined`: every enabled one) and a profile. */
export interface Selection {
    readonly target?: string;
    readonly profile?: Profile;
}

/** `kargo build`, with the target and profile of `selection`. */
export function buildArgs(selection: Selection = {}): string[] {
    return ['build', ...targetArgs(selection.target), ...releaseArgs(selection.profile)];
}

/** `kargo check --tests`: the fastest feedback, for the target and profile of `selection`. */
export function checkArgs(selection: Selection = {}): string[] {
    return ['check', '--tests', ...targetArgs(selection.target), ...releaseArgs(selection.profile)];
}

/** `kargo clean`. */
export function cleanArgs(): string[] {
    return ['clean'];
}

/**
 * `kargo test`, optionally filtered to `package.Class.method` names containing `filter`, in the
 * package `pkg` of a workspace.
 */
export function testArgs(filter: string | undefined, selection: Selection = {}, pkg?: string): string[] {
    return ['test', ...packageArgs(pkg), ...targetArgs(selection.target), ...releaseArgs(selection.profile), ...filterArgs(filter)];
}

/**
 * `kargo test` for the Test explorer: JSON messages on stdout, every target tested even when one
 * fails, and `target` and `filter` narrowing the run.
 */
export function testExplorerArgs(filter: string | undefined, target: string | undefined): string[] {
    return ['test', '--message-format', 'json', '--no-fail-fast', ...targetArgs(target), ...filterArgs(filter)];
}

/** `kargo test --list --format json`: the tests of the project, without running them. */
export function testListArgs(): string[] {
    return ['test', '--list', '--format', 'json'];
}

/** What a Run button or a Run Program pick starts. */
export type RunTarget =
    | { readonly kind: 'program'; readonly target?: string }
    | { readonly kind: 'bin'; readonly name: string }
    | { readonly kind: 'example'; readonly name: string };

/**
 * `kargo run` for a program, a `[[bin]]` or an example, with the profile of `selection`, in the
 * package `pkg` of a workspace.
 */
export function runArgs(run: RunTarget, selection: Selection = {}, pkg?: string): string[] {
    const profile = releaseArgs(selection.profile);
    const packaged = packageArgs(pkg);
    switch (run.kind) {
        case 'bin':
            return ['run', ...packaged, '--bin', run.name, ...profile];
        case 'example':
            return ['run', ...packaged, '--example', run.name, ...profile];
        case 'program':
            return ['run', ...packaged, ...targetArgs(run.target ?? selection.target), ...profile];
    }
}

/** `kargo task <name>`: a task of `kargo.toml`. */
export function taskArgs(name: string): string[] {
    return ['task', name];
}

/** `kargo add <coordinates>`, into `[dev-dependencies]` with `dev`. */
export function addArgs(coordinates: string, dev: boolean): string[] {
    return ['add', ...(dev ? ['--dev'] : []), coordinates];
}

/** `kargo remove <name>`, from `[dev-dependencies]` with `dev`. */
export function removeArgs(name: string, dev: boolean): string[] {
    return ['remove', ...(dev ? ['--dev'] : []), name];
}

/** `kargo fetch --sources`: the dependencies and their sources jars, for navigation. */
export function fetchArgs(): string[] {
    return ['fetch', '--sources'];
}

/** `kargo update`: the newest versions the `kargo.toml` ranges allow. */
export function updateArgs(): string[] {
    return ['update'];
}

/** The command line as a person would type it, for titles and the output channel. */
export function describe(args: readonly string[]): string {
    return ['kargo', ...args].map(word => (/[\s"']/.test(word) ? `"${word.replace(/"/g, '\\"')}"` : word)).join(' ');
}

function packageArgs(pkg: string | undefined): string[] {
    return pkg ? ['-p', pkg] : [];
}

function targetArgs(target: string | undefined): string[] {
    return target ? ['-t', target] : [];
}

function releaseArgs(profile: Profile | undefined): string[] {
    return profile === 'release' ? ['--release'] : [];
}

function filterArgs(filter: string | undefined): string[] {
    return filter ? [filter] : [];
}
