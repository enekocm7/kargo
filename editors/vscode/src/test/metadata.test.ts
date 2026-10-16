import { test } from 'node:test';
import * as assert from 'node:assert/strict';
import * as path from 'node:path';
import { binOf, enabledTargets, isTestSource, packageOf, parseMetadata, targetOf } from '../metadata';

const root = path.join('home', 'me', 'app');
const lib = path.join(root, 'lib');

/** The output of `kargo metadata --no-deps` for a workspace with an app and a path dependency. */
const json = {
    version: 1,
    root,
    members: ['app'],
    packages: [
        {
            name: 'app',
            version: '0.1.0',
            member: true,
            root,
            manifest: path.join(root, 'kargo.toml'),
            bins: [{ name: 'server', path: path.join(root, 'src', 'Server.kt') }],
            examples: [{ name: 'greet', path: path.join(root, 'examples', 'greet.kt') }],
            tasks: [{ name: 'lint', description: 'Lint the sources' }, { description: 'no name' }],
            targets: [
                {
                    kind: 'jvm',
                    'source-roots': [path.join(root, 'src'), path.join(root, 'src-jvm')],
                    'test-roots': [path.join(root, 'tests'), path.join(root, 'tests-jvm')],
                    'main-class': 'com.example.app.MainKt',
                },
                {
                    kind: 'wasm',
                    'source-roots': [path.join(root, 'src'), path.join(root, 'src-wasm')],
                    'test-roots': [path.join(root, 'tests')],
                },
            ],
        },
        { name: 'lib', version: '1.0.0', member: false, root: lib, manifest: path.join(lib, 'kargo.toml'), targets: [{ kind: 'native' }] },
    ],
};

test('reads the packages, targets, programs and tasks, and ignores what it does not use', () => {
    const metadata = parseMetadata(json);
    assert.equal(metadata.root, root);
    assert.equal(metadata.packages.length, 2);
    const [app, dependency] = metadata.packages;
    assert.equal(app.name, 'app');
    assert.equal(app.member, true);
    assert.equal(app.targets[0].mainClass, 'com.example.app.MainKt');
    assert.deepEqual(app.targets[0].sourceRoots, [path.join(root, 'src'), path.join(root, 'src-jvm')]);
    assert.deepEqual(app.tasks, [{ name: 'lint', description: 'Lint the sources' }]);
    assert.equal(dependency.member, false);
    assert.deepEqual(dependency.targets[0].testRoots, []);
});

test('tolerates missing and mistyped fields', () => {
    const metadata = parseMetadata({ root: 7, packages: [null, { name: 'x', targets: 'jvm', bins: [{}] }] });
    assert.equal(metadata.root, '');
    assert.equal(metadata.packages.length, 2);
    assert.deepEqual(metadata.packages[1].targets, []);
    assert.deepEqual(metadata.packages[1].bins, []);
    assert.deepEqual(parseMetadata('not json'), { root: '', packages: [] });
});

test('lists the enabled targets once, in Kargo order', () => {
    assert.deepEqual(enabledTargets(parseMetadata(json)), ['jvm', 'native', 'wasm']);
});

test('finds the package of a file, and the target of its directory', () => {
    const metadata = parseMetadata(json);
    const wasmSource = path.join(root, 'src-wasm', 'Main.kt');
    assert.equal(packageOf(metadata, wasmSource)?.name, 'app');
    assert.equal(packageOf(metadata, path.join(lib, 'src', 'Lib.kt'))?.name, 'lib');
    assert.equal(packageOf(metadata, path.join('elsewhere', 'Main.kt')), undefined);
    const app = metadata.packages[0];
    assert.equal(targetOf(app, wasmSource), 'wasm');
    assert.equal(targetOf(app, path.join(root, 'tests-jvm', 'T.kt')), 'jvm');
    assert.equal(targetOf(app, path.join(root, 'src', 'Main.kt')), undefined);
});

test('recognizes test sources, bins and examples', () => {
    const app = parseMetadata(json).packages[0];
    assert.equal(isTestSource(app, path.join(root, 'tests', 'MathTest.kt')), true);
    assert.equal(isTestSource(app, path.join(root, 'src', 'Main.kt')), false);
    assert.equal(binOf(app, path.join(root, 'src', 'Server.kt'))?.name, 'server');
    assert.equal(binOf(app, path.join(root, 'src', 'Other.kt')), undefined);
});
