import { test } from 'node:test';
import * as assert from 'node:assert/strict';
import { parseMessage } from '../messages';

test('reads a discovered test with its source location', () => {
    const line = JSON.stringify({
        reason: 'test-discovered',
        'display-name': 'adds()',
        file: '/home/me/app/tests/MathTest.kt',
        id: '[engine:junit-jupiter]/[class:com.example.app.MathTest]/[method:adds()]',
        line: 7,
        name: 'adds',
        package: 'app',
        'parent-id': '[engine:junit-jupiter]/[class:com.example.app.MathTest]',
        suite: 'com.example.app.MathTest',
        target: 'jvm',
    });
    const message = parseMessage(line);
    assert.ok(message?.reason === 'test-discovered');
    assert.deepEqual(message.test, {
        target: 'jvm',
        id: '[engine:junit-jupiter]/[class:com.example.app.MathTest]/[method:adds()]',
        parentId: '[engine:junit-jupiter]/[class:com.example.app.MathTest]',
        package: 'app',
        suite: 'com.example.app.MathTest',
        name: 'adds',
        displayName: 'adds()',
        file: '/home/me/app/tests/MathTest.kt',
        line: 7,
    });
});

test('reads a failed test with its message and output', () => {
    const message = parseMessage(
        JSON.stringify({
            reason: 'test-finished',
            outcome: 'failed',
            'duration-ms': 12,
            message: 'expected 4',
            details: 'at MathTest.kt:9',
            stdout: 'hello\n',
            id: 'T1',
            name: 'adds',
            suite: 'app.MathTest',
            target: 'native',
        }),
    );
    assert.ok(message?.reason === 'test-finished');
    assert.equal(message.outcome, 'failed');
    assert.equal(message.durationMs, 12);
    assert.equal(message.message, 'expected 4');
    assert.equal(message.details, 'at MathTest.kt:9');
    assert.equal(message.stdout, 'hello\n');
    assert.equal(message.test.target, 'native');
    assert.equal(message.test.id, 'T1');
});

test('reads summaries and compiler messages', () => {
    assert.deepEqual(
        parseMessage('{"failed":1,"ignored":0,"package":"app","passed":3,"reason":"test-summary","success":false,"target":"jvm"}'),
        { reason: 'test-summary', target: 'jvm', success: false, passed: 3, failed: 1, ignored: 0 },
    );
    const compiler = parseMessage(
        '{"column":16,"file":"/a/Main.kt","line":2,"message":"bad","reason":"compiler-message","rendered":"src/Main.kt:2:16: error: bad","severity":"error","target":"jvm"}',
    );
    assert.ok(compiler?.reason === 'compiler-message');
    assert.equal(compiler.rendered, 'src/Main.kt:2:16: error: bad');
    assert.equal(compiler.file, '/a/Main.kt');
});

test('program output and broken JSON are not messages', () => {
    assert.equal(parseMessage('Hello, world!'), undefined);
    assert.equal(parseMessage('{"reason": '), undefined);
    assert.equal(parseMessage('{"reason":"something-new"}'), undefined);
    assert.equal(parseMessage('[1, 2]'), undefined);
});
