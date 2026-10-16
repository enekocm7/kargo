import { test } from 'node:test';
import * as assert from 'node:assert/strict';
import { DEFAULT_SEARCH_URL, parseHits, searchMaven, searchQuery, searchUrl } from '../maven';

test('a typed group and artifact prefix search the group and the artifact', () => {
    assert.equal(searchQuery('io.ktor:ktor-server-c'), 'g:io.ktor AND a:ktor-server-c*');
    assert.equal(searchQuery('io.ktor:'), 'g:io.ktor');
    assert.equal(searchQuery('kotlinx coroutines'), undefined);
});

test('words are searched as typed, and nothing is searched for one letter', () => {
    assert.equal(searchQuery('coroutines'), 'coroutines');
    assert.equal(searchQuery('  serialization  '), 'serialization');
    assert.equal(searchQuery('a'), undefined);
    assert.equal(searchQuery(''), undefined);
    assert.equal(searchQuery(':ktor'), undefined);
});

test('the URL encodes the query with + for spaces', () => {
    assert.equal(
        searchUrl(DEFAULT_SEARCH_URL, 'g:io.ktor AND a:ktor*'),
        'https://search.maven.org/solrsearch/select?q=g%3Aio.ktor+AND+a%3Aktor*&rows=20&wt=json&fl=id,g,a',
    );
});

test('reads the artifacts of a search response', () => {
    const body = {
        response: {
            docs: [
                { id: 'io.ktor:ktor-server-core-jvm', g: 'io.ktor', a: 'ktor-server-core-jvm' },
                { g: 'io.ktor' },
                { g: 'io.ktor', a: 'ktor-server-cio' },
            ],
        },
    };
    assert.deepEqual(parseHits(body), [
        { group: 'io.ktor', artifact: 'ktor-server-core-jvm' },
        { group: 'io.ktor', artifact: 'ktor-server-cio' },
    ]);
    assert.deepEqual(parseHits({}), []);
});

test('searchMaven asks the search API and gives nothing when the request fails', async () => {
    const asked: string[] = [];
    const hits = await searchMaven('io.ktor:ktor-server-c', 'https://mirror.example/select', async url => {
        asked.push(url);
        return { response: { docs: [{ g: 'io.ktor', a: 'ktor-server-cio' }] } };
    });
    assert.deepEqual(hits, [{ group: 'io.ktor', artifact: 'ktor-server-cio' }]);
    assert.match(asked[0], /^https:\/\/mirror\.example\/select\?q=g%3Aio\.ktor\+AND\+a%3Aktor-server-c\*/);

    const failed = await searchMaven('coroutines', DEFAULT_SEARCH_URL, async () => {
        throw new Error('offline');
    });
    assert.deepEqual(failed, []);
    assert.deepEqual(await searchMaven('x', DEFAULT_SEARCH_URL, async () => ({})), []);
});
