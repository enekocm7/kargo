//! Searching Maven Central for the Add Dependency pick, with the same queries as the completion of
//! coordinates in `kargo.toml`: `group:artifact-prefix` once a `:` is typed, else the words typed.

/** Maven Central's search API, the one `kargo lsp` uses. */
export const DEFAULT_SEARCH_URL = 'https://search.maven.org/solrsearch/select';

/** Overrides [DEFAULT_SEARCH_URL], as for `kargo lsp`: a mirror of the search API. */
export const SEARCH_URL_ENV = 'KARGO_MAVEN_SEARCH_URL';

const ROWS = 20;

/** An artifact the search found. */
export interface Hit {
    readonly group: string;
    readonly artifact: string;
}

/** The search query for what was typed, or `undefined` when nothing useful was typed yet. */
export function searchQuery(partial: string): string | undefined {
    const text = partial.trim();
    const valid = (part: string) => /^[A-Za-z0-9._-]+$/.test(part);
    const colon = text.indexOf(':');
    if (colon === -1) return text.length >= 2 && valid(text) ? text : undefined;
    const group = text.slice(0, colon);
    const artifact = text.slice(colon + 1);
    if (group === '' || !valid(group) || (artifact !== '' && !valid(artifact))) return undefined;
    return artifact === '' ? `g:${group}` : `g:${group} AND a:${artifact}*`;
}

/** The request URL for `query`. `base` is [DEFAULT_SEARCH_URL] or a mirror of it. */
export function searchUrl(base: string, query: string): string {
    const encoded = encodeURIComponent(query).replace(/%20/g, '+');
    return `${base}?q=${encoded}&rows=${ROWS}&wt=json&fl=id,g,a`;
}

/** The artifacts in a search response, best first, as the API returns them. */
export function parseHits(body: unknown): Hit[] {
    const docs = (body as { response?: { docs?: unknown } } | null)?.response?.docs;
    if (!Array.isArray(docs)) return [];
    return docs.flatMap((doc: { g?: unknown; a?: unknown }) =>
        typeof doc.g === 'string' && typeof doc.a === 'string' ? [{ group: doc.g, artifact: doc.a }] : [],
    );
}

/**
 * The artifacts matching `partial`, or none when nothing was typed or the search failed. `fetchJson`
 * does the request, so tests can answer it without the network.
 */
export async function searchMaven(
    partial: string,
    base: string,
    fetchJson: (url: string) => Promise<unknown>,
): Promise<Hit[]> {
    const query = searchQuery(partial);
    if (query === undefined) return [];
    try {
        return parseHits(await fetchJson(searchUrl(base, query)));
    } catch {
        return [];
    }
}
