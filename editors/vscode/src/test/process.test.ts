import { test } from 'node:test';
import * as assert from 'node:assert/strict';
import { LineReader, runProcess } from '../process';

test('line reader splits chunks at line endings and keeps the text', () => {
    const lines: string[] = [];
    const reader = new LineReader(line => lines.push(line));
    reader.push('first\r\nsec');
    reader.push('ond\nthird');
    assert.deepEqual(lines, ['first', 'second']);
    reader.end();
    assert.deepEqual(lines, ['first', 'second', 'third']);
    assert.equal(reader.text, 'first\r\nsecond\nthird');
});

test('runs a program and streams its lines, with its exit code', async () => {
    const out: string[] = [];
    const err: string[] = [];
    const outcome = await runProcess(
        process.execPath,
        ['-e', 'console.log("one"); console.log("two"); console.error("oops"); process.exit(3)'],
        { cwd: process.cwd(), onStdoutLine: line => out.push(line), onStderrLine: line => err.push(line) },
    );
    assert.equal(outcome.code, 3);
    assert.deepEqual(out, ['one', 'two']);
    assert.deepEqual(err, ['oops']);
    assert.equal(outcome.stdout, 'one\ntwo\n');
});

test('a program that cannot be started rejects with its name', async () => {
    await assert.rejects(runProcess('definitely-not-a-kargo-binary', [], { cwd: process.cwd() }), /could not start `definitely-not-a-kargo-binary`/);
});
