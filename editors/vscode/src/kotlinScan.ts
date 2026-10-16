//! A scan of Kotlin source for what the editor marks with Run and Debug buttons: the package, the
//! classes, the top-level `fun main` and the test functions. Lines are matched by pattern, not
//! parsed: comments and string literals are blanked out first, so a `class` in a string is ignored.

/** A class, object or interface. `path` joins nested names with `$`, as the JVM names them. */
export interface KotlinClass {
    readonly path: string;
    /** 0-based. */
    readonly line: number;
}

/** A test function. `className` is the class it is declared in, `undefined` at top level. */
export interface KotlinTest {
    readonly name: string;
    /** 0-based. */
    readonly line: number;
    readonly className?: string;
}

export interface KotlinScan {
    readonly packageName?: string;
    readonly classes: readonly KotlinClass[];
    /** 0-based lines of the top-level `fun main`s. */
    readonly mains: readonly number[];
    readonly tests: readonly KotlinTest[];
}

const PACKAGE = /^\s*package\s+([\w.]+)/;
const CLASS = /\b(?:class|object|interface)\s+(`[^`]+`|[A-Za-z_]\w*)/;
const FUN = /\bfun\s+(?:<[^>]*>\s*)?(?:[\w.<>?]+\.)?(`[^`]+`|[A-Za-z_]\w*)\s*\(/;
const MAIN = /\bfun\s+main\s*\(/;
const TEST_ANNOTATION = /@(?:[\w.]+\.)?(?:Test|ParameterizedTest|RepeatedTest|TestFactory)\b/;

/** Scans Kotlin `source`. Line numbers are those of `source`. */
export function scanKotlin(source: string): KotlinScan {
    let packageName: string | undefined;
    const classes: KotlinClass[] = [];
    const mains: number[] = [];
    const tests: KotlinTest[] = [];
    // The classes whose bodies are open, innermost last; `depth` is the body's brace depth.
    const open: { path: string; depth: number }[] = [];
    let depth = 0;
    // A class header whose body has not opened yet, with the parentheses of its constructor so far.
    let pendingClass: { path: string; parens: number } | undefined;
    let pendingTest = false;

    stripNonCode(source)
        .split('\n')
        .forEach((text, line) => {
            packageName ??= PACKAGE.exec(text)?.[1];

            const declared = CLASS.exec(text);
            if (declared) {
                const parent = open.at(-1)?.path;
                const path = parent ? `${parent}$${unquote(declared[1])}` : unquote(declared[1]);
                pendingClass = { path, parens: 0 };
                classes.push({ path, line });
            }
            if (pendingClass) pendingClass.parens += parentheses(text);

            // An annotation applies to the next function; blank lines and other annotations may come between.
            if (TEST_ANNOTATION.test(text)) pendingTest = true;
            const fun = FUN.exec(text);
            if (pendingTest) {
                if (fun) {
                    tests.push({ name: unquote(fun[1]), line, className: open.at(-1)?.path });
                    pendingTest = false;
                } else if (text.trim() !== '' && !text.trim().startsWith('@')) {
                    pendingTest = false;
                }
            }
            if (depth === 0 && MAIN.test(text)) mains.push(line);

            for (const ch of text) {
                if (ch === '{') {
                    depth++;
                    if (pendingClass) {
                        open.push({ path: pendingClass.path, depth });
                        pendingClass = undefined;
                    }
                } else if (ch === '}') {
                    if (open.at(-1)?.depth === depth) open.pop();
                    depth--;
                }
            }
            // A header with no body (`data class Point(val x: Int)`) ends on its line.
            if (pendingClass && !text.includes('{') && pendingClass.parens <= 0 && !/[:,(]\s*$/.test(text)) {
                pendingClass = undefined;
            }
        });

    return { packageName, classes, mains, tests };
}

/**
 * `source` with the contents of comments and string literals replaced by spaces. Newlines stay, so
 * line numbers don't change, and the code around them keeps its columns.
 */
export function stripNonCode(source: string): string {
    const out: string[] = [];
    const blank = (ch: string) => (ch === '\n' ? '\n' : ' ');
    let i = 0;
    while (i < source.length) {
        const ch = source[i];
        if (ch === '/' && source[i + 1] === '/') {
            while (i < source.length && source[i] !== '\n') out.push(blank(source[i++]));
        } else if (ch === '/' && source[i + 1] === '*') {
            // Kotlin block comments nest.
            let level = 0;
            do {
                if (source[i] === '/' && source[i + 1] === '*') {
                    level++;
                    out.push('  ');
                    i += 2;
                } else if (source[i] === '*' && source[i + 1] === '/') {
                    level--;
                    out.push('  ');
                    i += 2;
                } else {
                    out.push(blank(source[i++]));
                }
            } while (level > 0 && i < source.length);
        } else if (source.startsWith('"""', i)) {
            out.push('   ');
            i += 3;
            while (i < source.length && !source.startsWith('"""', i)) out.push(blank(source[i++]));
            if (i < source.length) {
                out.push('   ');
                i += 3;
            }
        } else if (ch === '"') {
            out.push(' ');
            i++;
            while (i < source.length && source[i] !== '"' && source[i] !== '\n') {
                const escaped = source[i] === '\\' && source[i + 1] !== '\n';
                out.push(' ');
                i += escaped ? 2 : 1;
                if (escaped) out.push(' ');
            }
            if (source[i] === '"') {
                out.push(' ');
                i++;
            }
        } else if (ch === "'" && charLiteralEnd(source, i) !== -1) {
            for (const end = charLiteralEnd(source, i); i <= end; i++) out.push(' ');
        } else {
            out.push(ch);
            i++;
        }
    }
    return out.join('');
}

/** The index of the `'` closing the character literal that opens at `at` (`'x'`, `'\n'`), or -1. */
function charLiteralEnd(source: string, at: number): number {
    const end = source.indexOf("'", at + (source[at + 1] === '\\' ? 3 : 2));
    return end !== -1 && end - at <= 4 && !source.slice(at, end).includes('\n') ? end : -1;
}

function parentheses(text: string): number {
    let balance = 0;
    for (const ch of text) {
        if (ch === '(') balance++;
        else if (ch === ')') balance--;
    }
    return balance;
}

function unquote(name: string): string {
    return name.replace(/^`|`$/g, '');
}
