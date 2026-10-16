//! The JSON messages of `kargo test --message-format json` and `kargo test --list --format json`,
//! one per line on stdout (see the machine-readable output reference). Lines that aren't Kargo's
//! messages, such as a program's own output, parse to `undefined`.

/** A test as Kargo names it. `id` identifies it within its target. */
export interface TestRef {
    readonly target: string;
    readonly id: string;
    readonly parentId?: string;
    readonly package?: string;
    /** The test class, such as `com.example.app.MathTest` (nested classes use `$`). */
    readonly suite?: string;
    /** The test function, such as `adds`. */
    readonly name?: string;
    /** The name the framework shows, such as `adds()`. */
    readonly displayName?: string;
}

/** A test from discovery, with the source it was declared in. */
export interface Discovered extends TestRef {
    readonly file?: string;
    /** 1-based. */
    readonly line?: number;
}

export type KargoMessage =
    | { readonly reason: 'test-discovered'; readonly test: Discovered }
    | { readonly reason: 'test-started'; readonly test: TestRef }
    | {
          readonly reason: 'test-finished';
          readonly test: TestRef;
          readonly outcome: 'passed' | 'failed' | 'ignored';
          readonly durationMs?: number;
          readonly message?: string;
          readonly details?: string;
          readonly stdout?: string;
          readonly stderr?: string;
      }
    | {
          readonly reason: 'test-summary';
          readonly target: string;
          readonly success: boolean;
          readonly passed: number;
          readonly failed: number;
          readonly ignored: number;
      }
    | {
          readonly reason: 'compiler-message';
          readonly severity: string;
          readonly message: string;
          readonly rendered: string;
          readonly file?: string;
          readonly line?: number;
          readonly column?: number;
          readonly target?: string;
      }
    | { readonly reason: 'build-finished'; readonly success: boolean };

type Json = Record<string, unknown>;

/** The message on one line of Kargo's JSON output, or `undefined` for any other line. */
export function parseMessage(line: string): KargoMessage | undefined {
    const trimmed = line.trim();
    if (!trimmed.startsWith('{')) return undefined;
    let json: Json;
    try {
        json = asObject(JSON.parse(trimmed));
    } catch {
        return undefined;
    }
    switch (json.reason) {
        case 'test-discovered':
            return { reason: 'test-discovered', test: { ...testRef(json), file: string(json.file), line: number(json.line) } };
        case 'test-started':
            return { reason: 'test-started', test: testRef(json) };
        case 'test-finished':
            return {
                reason: 'test-finished',
                test: testRef(json),
                outcome: outcomeOf(json.outcome),
                durationMs: number(json['duration-ms']),
                message: string(json.message),
                details: string(json.details),
                stdout: string(json.stdout),
                stderr: string(json.stderr),
            };
        case 'test-summary':
            return {
                reason: 'test-summary',
                target: string(json.target) ?? '',
                success: json.success === true,
                passed: number(json.passed) ?? 0,
                failed: number(json.failed) ?? 0,
                ignored: number(json.ignored) ?? 0,
            };
        case 'compiler-message':
            return {
                reason: 'compiler-message',
                severity: string(json.severity) ?? 'error',
                message: string(json.message) ?? '',
                rendered: string(json.rendered) ?? string(json.message) ?? '',
                file: string(json.file),
                line: number(json.line),
                column: number(json.column),
                target: string(json.target),
            };
        case 'build-finished':
            return { reason: 'build-finished', success: json.success === true };
        default:
            return undefined;
    }
}

/** The test a message names, with the identifying fields of [TestRef]. */
function testRef(json: Json): TestRef {
    return {
        target: string(json.target) ?? '',
        id: string(json.id) ?? `${string(json.suite) ?? ''}.${string(json.name) ?? ''}`,
        parentId: string(json['parent-id']),
        package: string(json.package),
        suite: string(json.suite),
        name: string(json.name),
        displayName: string(json['display-name']),
    };
}

function outcomeOf(value: unknown): 'passed' | 'failed' | 'ignored' {
    return value === 'failed' || value === 'ignored' ? value : 'passed';
}

function asObject(value: unknown): Json {
    return typeof value === 'object' && value !== null && !Array.isArray(value) ? (value as Json) : {};
}

function string(value: unknown): string | undefined {
    return typeof value === 'string' ? value : undefined;
}

function number(value: unknown): number | undefined {
    return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}
