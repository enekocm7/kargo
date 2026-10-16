import { test } from 'node:test';
import * as assert from 'node:assert/strict';
import { scanKotlin, stripNonCode } from '../kotlinScan';

test('finds the package, the top-level main and the classes', () => {
    const scan = scanKotlin(
        [
            'package com.example.app',
            '',
            'fun main(args: Array<String>) {',
            '    println("hi")',
            '}',
            '',
            'class Greeter {',
            '    fun greet() = "hi"',
            '}',
            'object Config',
            '',
        ].join('\n'),
    );
    assert.equal(scan.packageName, 'com.example.app');
    assert.deepEqual(scan.mains, [2]);
    assert.deepEqual(scan.classes, [
        { path: 'Greeter', line: 6 },
        { path: 'Config', line: 9 },
    ]);
});

test('a main inside a class is not the program', () => {
    const scan = scanKotlin(['object Tools {', '    fun main() {}', '}'].join('\n'));
    assert.deepEqual(scan.mains, []);
});

test('finds test functions with the class they are declared in', () => {
    const scan = scanKotlin(
        [
            'package app',
            'import org.junit.jupiter.api.Test',
            'class MathTest {',
            '    @Test',
            '    fun adds() {',
            '    }',
            '    @Test @DisplayName("sub")',
            '    fun `subtracts numbers`() {}',
            '}',
        ].join('\n'),
    );
    assert.deepEqual(scan.tests, [
        { name: 'adds', line: 4, className: 'MathTest' },
        { name: 'subtracts numbers', line: 7, className: 'MathTest' },
    ]);
});

test('an annotation may be followed by other annotations and blank lines', () => {
    const scan = scanKotlin(['@Test', '', '@Tag("slow")', 'fun slow() {}'].join('\n'));
    assert.deepEqual(scan.tests, [{ name: 'slow', line: 3, className: undefined }]);
});

test('a test annotation that is not followed by a function is dropped at the next statement', () => {
    const scan = scanKotlin(['@Test', 'val answer = 42', 'fun helper() {}'].join('\n'));
    assert.deepEqual(scan.tests, []);
});

test('nested classes are named the way the JVM names them', () => {
    const scan = scanKotlin(
        ['class Outer {', '    class Inner {', '        @Test fun nested() {}', '    }', '}'].join('\n'),
    );
    assert.deepEqual(scan.classes, [
        { path: 'Outer', line: 0 },
        { path: 'Outer$Inner', line: 1 },
    ]);
    assert.deepEqual(scan.tests, [{ name: 'nested', line: 2, className: 'Outer$Inner' }]);
});

test('a class header that spans lines and a header without a body', () => {
    const multiline = scanKotlin(['class Config(', '    val port: Int,', ') {', '    @Test fun port() {}', '}'].join('\n'));
    assert.deepEqual(multiline.classes, [{ path: 'Config', line: 0 }]);
    assert.deepEqual(multiline.tests, [{ name: 'port', line: 3, className: 'Config' }]);

    const bodyless = scanKotlin(['data class Point(val x: Int)', 'fun main() {', '}'].join('\n'));
    assert.deepEqual(bodyless.classes, [{ path: 'Point', line: 0 }]);
    assert.deepEqual(bodyless.mains, [1]);
});

test('comments and strings do not count', () => {
    const scan = scanKotlin(
        [
            '// class Fake {',
            '/* fun main() {',
            '   class AlsoFake */',
            'val text = "class Quoted { @Test fun x() }"',
            'val raw = """',
            'fun main() {',
            '"""',
            'val c = \'{\'',
            'fun main() {}',
        ].join('\n'),
    );
    assert.deepEqual(scan.classes, []);
    assert.deepEqual(scan.mains, [8]);
    assert.deepEqual(scan.tests, []);
});

test('block comments nest, as they do in Kotlin', () => {
    assert.equal(stripNonCode('/* a /* b */ still comment */ fun x()').trim(), 'fun x()');
});

test('stripNonCode keeps line numbers and columns', () => {
    const source = 'val s = "a\\"b" // tail\nfun f() {}';
    const stripped = stripNonCode(source);
    assert.equal(stripped.split('\n').length, 2);
    assert.equal(stripped.length, source.length);
    assert.equal(stripped.indexOf('fun'), source.indexOf('fun'));
});
