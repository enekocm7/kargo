//! The project as `kargo metadata --no-deps` describes it: the packages, their targets, programs,
//! examples and tasks. The extension reads only the fields it uses and ignores the others, as the
//! format allows (see the versioning notes in the machine-readable output reference).

import * as path from 'node:path';

/** The whole project: the packages of a workspace, or the one package. */
export interface Metadata {
    /** The workspace root, or the package's directory. */
    readonly root: string;
    readonly packages: readonly Package[];
}

export interface Package {
    readonly name: string;
    readonly version: string;
    /** `false` for a package reached through a path dependency. */
    readonly member: boolean;
    /** Absolute directory of the package. */
    readonly root: string;
    readonly manifest: string;
    readonly targets: readonly Target[];
    readonly bins: readonly Named[];
    readonly examples: readonly Named[];
    readonly tasks: readonly Task[];
}

export interface Target {
    /** `jvm`, `wasm`, `native` or `android`. */
    readonly kind: string;
    /** Absolute source directories (`[layout] src` and `[layout.<target>] src`). */
    readonly sourceRoots: readonly string[];
    /** Absolute test directories (`[layout] tests` and `[layout.<target>] tests`). */
    readonly testRoots: readonly string[];
    readonly mainClass?: string;
}

/** A `[[bin]]` or an example: `path` is the source file, absolute, when known. */
export interface Named {
    readonly name: string;
    readonly path?: string;
}

export interface Task {
    readonly name: string;
    readonly description?: string;
}

/** The targets Kargo knows, in the order `kargo run` picks its default. */
export const TARGETS = ['jvm', 'native', 'wasm', 'android'] as const;

type Json = Record<string, unknown>;

/** Reads `kargo metadata` output. Anything missing or of the wrong type becomes an empty value. */
export function parseMetadata(json: unknown): Metadata {
    const root = asObject(json);
    return {
        root: text(root.root) ?? '',
        packages: list(root.packages).map(p => parsePackage(asObject(p))),
    };
}

function parsePackage(pkg: Json): Package {
    return {
        name: text(pkg.name) ?? '',
        version: text(pkg.version) ?? '',
        member: pkg.member !== false,
        root: text(pkg.root) ?? '',
        manifest: text(pkg.manifest) ?? '',
        targets: list(pkg.targets).map(t => parseTarget(asObject(t))),
        bins: list(pkg.bins).map(b => parseNamed(asObject(b))).filter(named => named.name !== ''),
        examples: list(pkg.examples).map(e => parseNamed(asObject(e))).filter(named => named.name !== ''),
        tasks: list(pkg.tasks)
            .map(t => asObject(t))
            .map(t => ({ name: text(t.name) ?? '', description: text(t.description) }))
            .filter(task => task.name !== ''),
    };
}

function parseTarget(target: Json): Target {
    return {
        kind: text(target.kind) ?? '',
        sourceRoots: strings(target['source-roots']),
        testRoots: strings(target['test-roots']),
        mainClass: text(target['main-class']),
    };
}

function parseNamed(entry: Json): Named {
    return { name: text(entry.name) ?? '', path: text(entry.path) };
}

/** The targets enabled anywhere in the project, in Kargo's order. */
export function enabledTargets(metadata: Metadata): string[] {
    const kinds = new Set(metadata.packages.flatMap(p => p.targets.map(t => t.kind)));
    return TARGETS.filter(kind => kinds.has(kind));
}

/** The targets a package enables, in Kargo's order. */
export function packageTargets(pkg: Package): string[] {
    const kinds = new Set(pkg.targets.map(t => t.kind));
    return TARGETS.filter(kind => kinds.has(kind));
}

/** The package whose directory holds `file`, the innermost one when packages are nested. */
export function packageOf(metadata: Metadata, file: string): Package | undefined {
    let found: Package | undefined;
    for (const pkg of metadata.packages) {
        if (!isInside(file, pkg.root)) continue;
        if (found === undefined || pkg.root.length > found.root.length) found = pkg;
    }
    return found;
}

/**
 * The target a source file belongs to: the only target whose `sourceRoots` or `testRoots`
 * hold it, or `undefined` for `src/` and `tests/` (and other common directories), which
 * every target compiles. Reads the directories from `kargo metadata`, so `[layout]`
 * directories work too; the `src-<target>` name is only a fallback.
 */
export function targetOf(pkg: Package, file: string): string | undefined {
    const holders = pkg.targets.filter(target =>
        [...target.sourceRoots, ...target.testRoots].some(root => isInside(file, root)),
    );
    if (holders.length === 1) return holders[0].kind;
    if (holders.length > 1) return undefined;
    const top = path.relative(pkg.root, file).split(/[\\/]/)[0] ?? '';
    const match = /^(?:src|tests|test-fixtures)-(jvm|wasm|js|native|android)$/.exec(top);
    return match?.[1];
}

/** Whether `file` is under one of the package's test directories. */
export function isTestSource(pkg: Package, file: string): boolean {
    return pkg.targets.some(target => target.testRoots.some(root => isInside(file, root)));
}

/** The `[[bin]]` whose source is `file`, if any. */
export function binOf(pkg: Package, file: string): Named | undefined {
    return pkg.bins.find(bin => bin.path !== undefined && samePath(bin.path, file));
}

/** The example whose source is `file`, if any. */
export function exampleOf(pkg: Package, file: string): Named | undefined {
    return pkg.examples.find(example => example.path !== undefined && samePath(example.path, file));
}

/** Whether `file` is `directory` or lies below it. */
export function isInside(file: string, directory: string): boolean {
    if (directory === '') return false;
    const relative = path.relative(directory, file);
    return relative === '' || (!relative.startsWith('..') && !path.isAbsolute(relative));
}

function samePath(a: string, b: string): boolean {
    return path.relative(a, b) === '';
}

function asObject(value: unknown): Json {
    return typeof value === 'object' && value !== null && !Array.isArray(value) ? (value as Json) : {};
}

function list(value: unknown): unknown[] {
    return Array.isArray(value) ? value : [];
}

function text(value: unknown): string | undefined {
    return typeof value === 'string' ? value : undefined;
}

function strings(value: unknown): string[] {
    return list(value).filter((entry): entry is string => typeof entry === 'string');
}
