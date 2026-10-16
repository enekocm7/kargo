//! Running `kargo` and reading what it prints, line by line. The output of short commands (metadata,
//! test discovery, test runs) comes back whole, and streams to the callers as it arrives.

import { spawn } from 'node:child_process';

export interface Outcome {
    /** The exit code, or `null` when the process was killed. */
    readonly code: number | null;
    readonly stdout: string;
    readonly stderr: string;
}

export interface RunOptions {
    readonly cwd: string;
    /** Kills the process when aborted. */
    readonly signal?: AbortSignal;
    /** Each line of stdout, without its line ending, as it arrives. */
    readonly onStdoutLine?: (line: string) => void;
    /** Each line of stderr, without its line ending, as it arrives. */
    readonly onStderrLine?: (line: string) => void;
}

/**
 * Runs `program` with `args` in `options.cwd` and resolves when it exits. It rejects only when the
 * program can't be started; a failing exit code is in the outcome.
 */
export function runProcess(program: string, args: readonly string[], options: RunOptions): Promise<Outcome> {
    return new Promise((resolve, reject) => {
        const child = spawn(program, [...args], {
            cwd: options.cwd,
            windowsHide: true,
            stdio: ['ignore', 'pipe', 'pipe'],
            signal: options.signal,
        });
        const stdout = new LineReader(options.onStdoutLine);
        const stderr = new LineReader(options.onStderrLine);
        child.stdout.setEncoding('utf8');
        child.stderr.setEncoding('utf8');
        child.stdout.on('data', (chunk: string) => stdout.push(chunk));
        child.stderr.on('data', (chunk: string) => stderr.push(chunk));
        child.on('error', error => reject(new Error(`could not start \`${program}\`: ${error.message}`)));
        child.on('close', code => {
            stdout.end();
            stderr.end();
            resolve({ code, stdout: stdout.text, stderr: stderr.text });
        });
    });
}

/** Splits streamed text into lines, keeping the whole text too. */
export class LineReader {
    private pending = '';
    private all = '';

    constructor(private readonly onLine?: (line: string) => void) {}

    /** The text so far. */
    get text(): string {
        return this.all;
    }

    push(chunk: string): void {
        this.all += chunk;
        this.pending += chunk;
        let end = this.pending.indexOf('\n');
        while (end !== -1) {
            this.emit(this.pending.slice(0, end));
            this.pending = this.pending.slice(end + 1);
            end = this.pending.indexOf('\n');
        }
    }

    /** Emits what is left without a line ending. */
    end(): void {
        if (this.pending !== '') this.emit(this.pending);
        this.pending = '';
    }

    private emit(line: string): void {
        this.onLine?.(line.replace(/\r$/, ''));
    }
}
