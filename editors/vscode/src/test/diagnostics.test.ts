import { test } from 'node:test';
import * as assert from 'node:assert/strict';
import { parseCompilerLine, stripAnsi } from '../diagnostics';

test('reads a Kotlin error with its location', () => {
    assert.deepEqual(parseCompilerLine('src/Main.kt:2:16: error: initializer type mismatch: expected Int'), {
        severity: 'error',
        file: 'src/Main.kt',
        line: 2,
        column: 16,
        message: 'initializer type mismatch: expected Int',
    });
});

test('reads warnings and infos, and keeps the message as written', () => {
    const warning = parseCompilerLine("src/A.kt:1:1: warning: 'f' is deprecated: use g: it");
    assert.equal(warning?.severity, 'warning');
    assert.equal(warning?.message, "'f' is deprecated: use g: it");
    assert.equal(parseCompilerLine('src/A.kt:3:1: info: hint')?.severity, 'info');
});

test('reads javac diagnostics, which have no column', () => {
    assert.deepEqual(parseCompilerLine('C:\\app\\src\\Util.java:5: error: \';\' expected'), {
        severity: 'error',
        file: 'C:\\app\\src\\Util.java',
        line: 5,
        message: "';' expected",
    });
});

test('handles CRLF line endings and terminal colours', () => {
    assert.equal(parseCompilerLine('\u001b[31msrc/A.kt:1:2: error: boom\u001b[0m\r')?.message, 'boom');
    assert.equal(stripAnsi('\u001b[1mbold\u001b[0m'), 'bold');
});

test('ignores lines without a location', () => {
    assert.equal(parseCompilerLine('error: could not build the wasm target'), undefined);
    assert.equal(parseCompilerLine('Running `java com.example.MainKt`'), undefined);
});
