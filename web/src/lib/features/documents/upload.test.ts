import { describe, expect, it } from 'vitest';
import { uploadFile } from './upload';

/**
 * The most important test in this slice, and it is not really about uploading.
 *
 * `uploadFile` is the only code in the app that hands the browser a URL which writes to storage, and
 * the only code that makes a cross-origin request to a host we do not control. If a session token
 * ever rides along on that request, invariant 20 is dead and nothing else in the codebase would
 * notice — the upload would still work.
 */

interface Call {
	url: string;
	init: RequestInit | undefined;
}

function headersOf(init: RequestInit | undefined): Record<string, string> {
	const h = init?.headers;
	if (!h) return {};
	const out: Record<string, string> = {};
	for (const [k, v] of Object.entries(h as Record<string, string>)) out[k.toLowerCase()] = v;
	return out;
}

/** Records every call, and answers the mint and the PUT. */
function stubFetch(opts: {
	putStatus?: number | number[];
	mintStatus?: number;
	mintBody?: unknown;
}) {
	const calls: Call[] = [];
	const putStatuses = Array.isArray(opts.putStatus) ? [...opts.putStatus] : [opts.putStatus ?? 200];

	const fetch = (async (url: string | URL, init?: RequestInit) => {
		calls.push({ url: String(url), init });

		if (String(url).startsWith('/documents/upload-url')) {
			const status = opts.mintStatus ?? 200;
			const body =
				opts.mintBody ??
				({
					documentId: 'doc-1',
					uploadUrl:
						'http://localhost:9000/documents/tenants/acme/documents/doc-1/original.pdf?X-Amz-Signature=abc',
					expiresAt: '2026-01-01T00:00:00Z'
				} satisfies Record<string, unknown>);
			return new Response(JSON.stringify(body), {
				status,
				headers: { 'content-type': 'application/json' }
			});
		}

		return new Response(null, { status: putStatuses.shift() ?? 200 });
	}) as unknown as typeof globalThis.fetch;

	return { fetch, calls };
}

function file(name = 'faq.pdf', size = 1024): File {
	return new File([new Uint8Array(size)], name, { type: 'application/pdf' });
}

describe('uploadFile — the invariant 20 boundary', () => {
	it('never sends a credential to storage', async () => {
		const { fetch, calls } = stubFetch({});
		const out = await uploadFile(file(), { fetch });
		expect(out).toEqual({ ok: true, documentId: 'doc-1' });

		const put = calls.find((c) => c.init?.method === 'PUT');
		expect(put, 'the PUT should have happened').toBeDefined();

		// The PUT is cross-origin, to a host we do not control. A session token here would be handed
		// to MinIO on every upload.
		expect(headersOf(put!.init)['authorization']).toBeUndefined();
		expect(headersOf(put!.init)['cookie']).toBeUndefined();
		// And no cookies, ever — the session cookie must not follow the bytes.
		expect(put!.init?.credentials).toBe('omit');
		expect(put!.init?.credentials).not.toBe('include');
	});

	it('asks our own origin for the url, relatively', async () => {
		const { fetch, calls } = stubFetch({});
		await uploadFile(file(), { fetch });

		// An absolute API URL here would mean the browser talking to the Rust API directly — which it
		// cannot authenticate to, because it does not have (and must not have) the session token.
		expect(calls[0].url).toBe('/documents/upload-url');
		expect(calls[0].url.startsWith('http')).toBe(false);
	});

	it('sends no authorization header to our own origin either — the cookie does that', async () => {
		const { fetch, calls } = stubFetch({});
		await uploadFile(file(), { fetch });
		expect(headersOf(calls[0].init)['authorization']).toBeUndefined();
	});
});

describe('uploadFile — validation short-circuits', () => {
	it('rejects an unsupported type without touching the network', async () => {
		const { fetch, calls } = stubFetch({});
		const out = await uploadFile(file('resume.docx'), { fetch });

		expect(out.ok).toBe(false);
		// No row should be created for a file we already know the sidecar cannot read.
		expect(calls).toHaveLength(0);
	});

	it('rejects a dotfile without touching the network', async () => {
		const { fetch, calls } = stubFetch({});
		const out = await uploadFile(file('.pdf'), { fetch });

		expect(out.ok).toBe(false);
		expect(calls).toHaveLength(0);
	});

	it('rejects an oversize file without touching the network', async () => {
		const { fetch, calls } = stubFetch({});
		const out = await uploadFile(file('big.pdf', 26 * 1024 * 1024), { fetch });

		expect(out.ok).toBe(false);
		expect(calls).toHaveLength(0);
	});
});

describe('uploadFile — the 403 re-mint, and its one safe use', () => {
	it('re-mints and retries once with the same file', async () => {
		const { fetch, calls } = stubFetch({ putStatus: [403, 200] });
		const out = await uploadFile(file(), { fetch });

		expect(out).toEqual({ ok: true, documentId: 'doc-1' });

		// The re-mint must be keyed by documentId, not filename: it re-signs the row's EXISTING object
		// key. Sending a filename would imply the key could change, which it cannot.
		const mints = calls.filter((c) => c.url === '/documents/upload-url');
		expect(mints).toHaveLength(2);
		expect(JSON.parse(mints[1].init!.body as string)).toEqual({ documentId: 'doc-1' });
	});

	it('gives up after a second 403 rather than hammering storage', async () => {
		const { fetch, calls } = stubFetch({ putStatus: [403, 403] });
		const out = await uploadFile(file(), { fetch });

		expect(out.ok).toBe(false);
		// Exactly two PUTs. A fresh URL cannot already be expired, so a second 403 is a signature
		// problem, and retrying it would loop forever.
		expect(calls.filter((c) => c.init?.method === 'PUT')).toHaveLength(2);
	});
});

describe('uploadFile — failures the user can act on', () => {
	it('surfaces a 401 as a session problem', async () => {
		const { fetch } = stubFetch({ mintStatus: 401, mintBody: {} });
		const out = await uploadFile(file(), { fetch });

		expect(out.ok).toBe(false);
		expect(!out.ok && out.message).toMatch(/log in again/i);
	});

	it("never surfaces the api's raw internal message", async () => {
		const { fetch } = stubFetch({
			mintStatus: 500,
			mintBody: {
				error: { status: 500, message: 'RLS denied on documents', kind: 'server', path: '/x' }
			}
		});
		const out = await uploadFile(file(), { fetch });

		expect(out.ok).toBe(false);
		// Invariant 16: internal detail is logged, never shown.
		expect(!out.ok && out.message).not.toContain('RLS');
	});

	it('reports a dead network without throwing', async () => {
		const fetch = (async () => {
			throw new TypeError('Failed to fetch');
		}) as unknown as typeof globalThis.fetch;

		const out = await uploadFile(file(), { fetch });
		expect(out.ok).toBe(false);
		expect(!out.ok && out.message).toMatch(/connection/i);
	});
});
