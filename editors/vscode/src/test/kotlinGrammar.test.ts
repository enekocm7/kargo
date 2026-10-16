import { test } from 'node:test';
import * as assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import * as path from 'node:path';
import { createOnigScanner, createOnigString, loadWASM } from 'vscode-oniguruma';
import { INITIAL, IGrammar, Registry, parseRawGrammar } from 'vscode-textmate';

// The grammar the extension contributes, so the scopes below are the ones a theme colours.
const GRAMMAR = path.resolve(__dirname, '..', '..', 'syntaxes', 'kotlin.tmLanguage.json');

interface Token {
    readonly text: string;
    readonly scopes: readonly string[];
}

let grammar: IGrammar | undefined;

async function kotlin(): Promise<IGrammar> {
    if (grammar) return grammar;
    await loadWASM(await readFile(require.resolve('vscode-oniguruma/release/onig.wasm')));
    const registry = new Registry({
        onigLib: Promise.resolve({ createOnigScanner, createOnigString }),
        loadGrammar: async scopeName =>
            scopeName === 'source.kotlin' ? parseRawGrammar(await readFile(GRAMMAR, 'utf8'), GRAMMAR) : null,
    });
    grammar = (await registry.loadGrammar('source.kotlin')) ?? undefined;
    assert.ok(grammar, 'the Kotlin grammar loads');
    return grammar;
}

/** The tokens of each line of `source`, with the scopes a theme colours them by. */
async function tokenize(source: string): Promise<Token[][]> {
    const g = await kotlin();
    let state = INITIAL;
    return source.split('\n').map(line => {
        const result = g.tokenizeLine(line, state);
        state = result.ruleStack;
        return result.tokens.map(token => ({ text: line.slice(token.startIndex, token.endIndex), scopes: token.scopes }));
    });
}

/**
 * The scopes of the token that reads `word`. TextMate merges neighbouring tokens with the same scopes, so a plain
 * word is often inside a longer token (`" data = "`); then the first token containing it stands for it.
 */
function scopesOf(lines: Token[][], word: string): readonly string[] {
    const all = lines.flat();
    const token = all.find(candidate => candidate.text === word) ?? all.find(candidate => candidate.text.includes(word));
    if (!token) assert.fail(`no token reads \`${word}\``);
    return token.scopes;
}

function assertScope(lines: Token[][], word: string, scope: string): void {
    assert.ok(scopesOf(lines, word).includes(scope), `\`${word}\` has ${scope}, has ${scopesOf(lines, word).join(' ')}`);
}

test('colours declarations, keywords and types', async () => {
    const lines = await tokenize(
        ['package com.example.app', '', 'fun greeting(name: String): String = "Hello, $name!"', 'val answer: Int = 42'].join('\n'),
    );
    assertScope(lines, 'package', 'keyword.other.kotlin');
    assertScope(lines, 'fun', 'storage.type.function.kotlin');
    assertScope(lines, 'greeting', 'entity.name.function.kotlin');
    assertScope(lines, 'String', 'entity.name.type.kotlin');
    assertScope(lines, 'val', 'storage.type.kotlin');
    assertScope(lines, 'Int', 'entity.name.type.kotlin');
    assertScope(lines, '42', 'constant.numeric.kotlin');
});

test('colours strings, templates and escapes', async () => {
    const lines = await tokenize(['val s = "Hello, $name, ${count + 1}\\t!"', "val c = 'x'"].join('\n'));
    assertScope(lines, 'Hello, ', 'string.quoted.double.kotlin');
    assertScope(lines, '$name', 'variable.other.template.kotlin');
    assertScope(lines, '\\t', 'constant.character.escape.kotlin');
    assertScope(lines, "'x'", 'string.quoted.single.kotlin');
});

test('does not read code inside a string as code', async () => {
    const lines = await tokenize('val s = "class Fake { fun inside() }"');
    assert.ok(scopesOf(lines, 'class').includes('string.quoted.double.kotlin'));
    assert.ok(!scopesOf(lines, 'class').includes('storage.type.kotlin'));
});

test('colours comments, including a block comment across lines', async () => {
    const lines = await tokenize(['// class Fake', '/* first', 'second', '*/ val x = 1'].join('\n'));
    assertScope(lines, '// class Fake', 'comment.line.double-slash.kotlin');
    assertScope(lines, 'second', 'comment.block.kotlin');
    assertScope(lines, 'val', 'storage.type.kotlin');
});

test('a raw string keeps the code inside it out of the colours of code', async () => {
    const lines = await tokenize(['val raw = """', 'fun inside() {}', '"""', 'fun after() {}'].join('\n'));
    const inside = lines[1].map(token => token.text).join('');
    assert.equal(inside, 'fun inside() {}');
    assert.ok(lines[1].every(token => token.scopes.includes('string.quoted.triple.kotlin')), 'the body is string content');
    assertScope(lines, 'after', 'entity.name.function.kotlin');
});

test('colours annotations, modifiers and control keywords', async () => {
    const lines = await tokenize(
        ['@file:JvmName("App")', 'data class Point(val x: Int)', 'class Box(data: Int)', 'fun main() {', 'if (x) return null', '}'].join('\n'),
    );
    assertScope(lines, '@file:JvmName', 'storage.type.annotation.kotlin');
    assertScope(lines, 'data', 'storage.modifier.kotlin');
    assertScope(lines, 'Point', 'entity.name.type.kotlin');
    assertScope(lines, 'if', 'keyword.control.kotlin');
    assertScope(lines, 'return', 'keyword.control.kotlin');
    assertScope(lines, 'null', 'constant.language.kotlin');
});

test('a variable named like a modifier is not coloured as one', async () => {
    const lines = await tokenize('val data = value + 1');
    assert.ok(!scopesOf(lines, 'data').includes('storage.modifier.kotlin'));
    assert.ok(!scopesOf(lines, 'value').includes('storage.modifier.kotlin'));
});

test('colours calls and constants', async () => {
    const lines = await tokenize('println(greeting("Kotlin"))\nconst val MAX_SIZE = 10');
    assertScope(lines, 'println', 'entity.name.function.kotlin');
    assertScope(lines, 'MAX_SIZE', 'variable.other.constant.kotlin');
});
