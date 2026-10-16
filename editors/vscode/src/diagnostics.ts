//! Compiler diagnostics in Kargo's human output: the header line of each message, which the
//! Problems view needs. Kargo prints them as kotlinc and javac do, `path:line:column: severity:
//! message` for Kotlin and `path:line: severity: message` for javac.

/** One compiler message with a location. `file` is as the compiler printed it. */
export interface CompilerMessage {
    readonly severity: 'error' | 'warning' | 'info';
    readonly file: string;
    /** 1-based. */
    readonly line: number;
    /** 1-based; `undefined` for javac, which reports none. */
    readonly column?: number;
    readonly message: string;
}

const KOTLIN = /^(.+?):(\d+):(\d+): (error|warning|info|note): (.*)$/;
const JAVAC = /^(.+?):(\d+): (error|warning|note): (.*)$/;

/** The message on one line of compiler output, or `undefined` for any other line. */
export function parseCompilerLine(raw: string): CompilerMessage | undefined {
    const line = stripAnsi(raw).replace(/\r$/, '');
    const kotlin = KOTLIN.exec(line);
    if (kotlin) {
        return { severity: severityOf(kotlin[4]), file: kotlin[1], line: Number(kotlin[2]), column: Number(kotlin[3]), message: kotlin[5] };
    }
    const javac = JAVAC.exec(line);
    if (javac) {
        return { severity: severityOf(javac[3]), file: javac[1], line: Number(javac[2]), message: javac[4] };
    }
    return undefined;
}

function severityOf(word: string): CompilerMessage['severity'] {
    switch (word) {
        case 'error':
            return 'error';
        case 'warning':
            return 'warning';
        default:
            return 'info';
    }
}

/** The text without terminal colour codes. */
export function stripAnsi(text: string): string {
    return text.replace(/\x1b\[[0-9;]*[A-Za-z]/g, '');
}
