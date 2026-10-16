//! Finding the `kargo` executable: the `kargo.path` setting, else the `kargo` on `PATH`, else
//! `~/.cargo/bin`, where `rustup` installs it. Pure, so the lookup is unit tested without a disk.

import * as path from 'node:path';

/** What the lookup can see: the environment, the platform, and whether a file exists. */
export interface Lookup {
    readonly env: NodeJS.ProcessEnv;
    readonly platform: NodeJS.Platform;
    readonly home: string;
    isFile(file: string): boolean;
}

/**
 * The `kargo` to run. `configured` (the `kargo.path` setting) wins when it names a file;
 * otherwise `undefined` means no `kargo` was found.
 */
export function findKargo(configured: string, lookup: Lookup): string | undefined {
    const name = lookup.platform === 'win32' ? 'kargo.exe' : 'kargo';
    const explicit = configured.trim();
    if (explicit !== '') {
        // A bare name is looked up on PATH like the default; a path is taken as written.
        if (path.basename(explicit) !== explicit) return explicit;
        return searchPath(explicit, lookup);
    }
    return searchPath(name, lookup) ?? searchDirectory(path.join(lookup.home, '.cargo', 'bin'), name, lookup);
}

function searchPath(name: string, lookup: Lookup): string | undefined {
    const pathVariable = lookup.env.PATH ?? lookup.env.Path ?? '';
    for (const directory of pathVariable.split(path.delimiter)) {
        if (directory === '') continue;
        const found = searchDirectory(directory, name, lookup);
        if (found) return found;
    }
    return undefined;
}

function searchDirectory(directory: string, name: string, lookup: Lookup): string | undefined {
    const candidate = path.join(directory, name);
    return lookup.isFile(candidate) ? candidate : undefined;
}

/** The message shown when no `kargo` can be found. */
export const NOT_INSTALLED = 'Kargo is not installed: install it, or set its path in the `kargo.path` setting.';
