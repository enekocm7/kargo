import { test } from 'node:test';
import * as assert from 'node:assert/strict';
import * as path from 'node:path';
import { NOT_INSTALLED, Lookup, findKargo } from '../executable';

/** A lookup over the files in `files`, with a PATH of `directories`. */
function lookup(files: string[], options: { platform?: NodeJS.Platform; directories?: string[] } = {}): Lookup {
    return {
        platform: options.platform ?? 'linux',
        home: path.join('home', 'me'),
        env: { PATH: (options.directories ?? []).join(path.delimiter) },
        isFile: file => files.includes(file),
    };
}

test('a configured path is used as written, even when it does not exist yet', () => {
    const configured = path.join('opt', 'kargo', 'bin', 'kargo');
    assert.equal(findKargo(configured, lookup([])), configured);
    assert.equal(findKargo(`  ${configured}  `, lookup([])), configured);
});

test('a configured bare name is looked up on PATH', () => {
    const bin = path.join('usr', 'local', 'bin');
    assert.equal(findKargo('kargo', lookup([path.join(bin, 'kargo')], { directories: [bin] })), path.join(bin, 'kargo'));
});

test('finds kargo on PATH, in the first directory that holds it', () => {
    const first = 'a';
    const second = 'b';
    const files = [path.join(first, 'kargo'), path.join(second, 'kargo')];
    assert.equal(findKargo('', lookup(files, { directories: [first, second] })), path.join(first, 'kargo'));
});

test('falls back to ~/.cargo/bin when PATH has no kargo', () => {
    const cargoBin = path.join('home', 'me', '.cargo', 'bin', 'kargo');
    assert.equal(findKargo('', lookup([cargoBin], { directories: [path.join('usr', 'bin')] })), cargoBin);
});

test('uses kargo.exe on Windows', () => {
    const bin = 'tools';
    assert.equal(findKargo('', lookup([path.join(bin, 'kargo.exe')], { platform: 'win32', directories: [bin] })), path.join(bin, 'kargo.exe'));
    assert.equal(findKargo('', lookup([path.join(bin, 'kargo')], { platform: 'win32', directories: [bin] })), undefined);
});

test('reports nothing when kargo is nowhere', () => {
    assert.equal(findKargo('', lookup([])), undefined);
    assert.match(NOT_INSTALLED, /kargo\.path/);
});
