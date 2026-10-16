import { test } from 'node:test';
import * as assert from 'node:assert/strict';
import { addArgs, buildArgs, checkArgs, describe, fetchArgs, removeArgs, runArgs, taskArgs, testArgs, testExplorerArgs, testListArgs } from '../commands';

test('build, check and test take the target and profile of the selection', () => {
    assert.deepEqual(buildArgs(), ['build']);
    assert.deepEqual(buildArgs({ target: 'wasm', profile: 'release' }), ['build', '-t', 'wasm', '--release']);
    assert.deepEqual(checkArgs({ profile: 'dev' }), ['check', '--tests']);
    assert.deepEqual(testArgs('com.example.MathTest', { target: 'native' }), ['test', '-t', 'native', 'com.example.MathTest']);
});

test('the Test explorer asks for JSON messages and runs every target', () => {
    assert.deepEqual(testExplorerArgs(undefined, undefined), ['test', '--message-format', 'json', '--no-fail-fast']);
    assert.deepEqual(testExplorerArgs('app.Tests.adds', 'jvm'), ['test', '--message-format', 'json', '--no-fail-fast', '-t', 'jvm', 'app.Tests.adds']);
    assert.deepEqual(testListArgs(), ['test', '--list', '--format', 'json']);
});

test('run arguments name a program, a bin or an example', () => {
    assert.deepEqual(runArgs({ kind: 'program' }), ['run']);
    assert.deepEqual(runArgs({ kind: 'program', target: 'jvm' }, { target: 'wasm' }), ['run', '-t', 'jvm']);
    assert.deepEqual(runArgs({ kind: 'program' }, { target: 'wasm', profile: 'release' }), ['run', '-t', 'wasm', '--release']);
    assert.deepEqual(runArgs({ kind: 'bin', name: 'server' }), ['run', '--bin', 'server']);
    assert.deepEqual(runArgs({ kind: 'example', name: 'greet' }, { profile: 'release' }), ['run', '--example', 'greet', '--release']);
});

test('a workspace member names its package', () => {
    assert.deepEqual(runArgs({ kind: 'bin', name: 'server' }, {}, 'app'), ['run', '-p', 'app', '--bin', 'server']);
    assert.deepEqual(testArgs('app.MathTest', { target: 'jvm' }, 'app'), ['test', '-p', 'app', '-t', 'jvm', 'app.MathTest']);
});

test('dependency, task and fetch commands', () => {
    assert.deepEqual(addArgs('io.ktor:ktor-server-core', false), ['add', 'io.ktor:ktor-server-core']);
    assert.deepEqual(addArgs('org.junit:junit', true), ['add', '--dev', 'org.junit:junit']);
    assert.deepEqual(removeArgs('ktor', true), ['remove', '--dev', 'ktor']);
    assert.deepEqual(taskArgs('lint'), ['task', 'lint']);
    assert.deepEqual(fetchArgs(), ['fetch', '--sources']);
});

test('describe quotes the words that contain spaces', () => {
    assert.equal(describe(['test', 'com.example.MathTest']), 'kargo test com.example.MathTest');
    assert.equal(describe(['task', 'say hi']), 'kargo task "say hi"');
});
