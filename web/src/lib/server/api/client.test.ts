import { describe, expect, it, vi } from 'vitest';
import { createApiClient, type FetchFn } from './client';

/**
 * `createApiClient` takes `fetch` as an option and imports no env — which is precisely so this file
 * can exist without a running API or a `.env`.
 */
function clientWith(fetch: FetchFn, token?: string) {
	return createApiClient({ baseUrl: 'http://api.test', fetch, token });
}

const json = (status: number, body: unknown) =>
	new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});

describe('parsing the response', () => {
	it('treats a 204 as success with no body — this is what makes logout work', async () => {
		const fetch = vi.fn(async () => new Response(null, { status: 204 })) as unknown as FetchFn;
		const res = await clientWith(fetch).post('/auth/logout');
		expect(res.ok).toBe(true);
	});

	it('reads the API error envelope', async () => {
		const fetch = vi.fn(async () =>
			json(409, { error: 'an account with this email already exists' })
		) as unknown as FetchFn;

		const res = await clientWith(fetch).post('/auth/register', {});
		expect(res.ok).toBe(false);
		if (!res.ok) {
			expect(res.error.kind).toBe('client');
			expect(res.error.status).toBe(409);
			expect(res.error.message).toBe('an account with this email already exists');
		}
	});

	it('does not throw on a text/plain axum rejection', async () => {
		// Axum's extractor rejections bypass AppError and come back as text, not our JSON envelope.
		// Calling res.json() first would throw here and lose the body with it.
		const fetch = vi.fn(
			async () =>
				new Response('Failed to deserialize the JSON body into the target type', {
					status: 415,
					headers: { 'content-type': 'text/plain; charset=utf-8' }
				})
		) as unknown as FetchFn;

		const res = await clientWith(fetch).post('/auth/login', {});
		expect(res.ok).toBe(false);
		if (!res.ok) {
			expect(res.error.kind).toBe('malformed');
			expect(res.error.status).toBe(415);
		}
	});

	it('classifies a 5xx as server, not client', async () => {
		const fetch = vi.fn(async () =>
			json(500, { error: 'internal server error' })
		) as unknown as FetchFn;

		const res = await clientWith(fetch).get('/auth/me');
		expect(res.ok).toBe(false);
		if (!res.ok) expect(res.error.kind).toBe('server');
	});

	it('classifies an unreachable API as transport, with status 0', async () => {
		// hooks.server.ts depends on this: a transport failure must NOT be mistaken for a 401, or an
		// API blip would log every user out.
		const fetch = vi.fn(async () => {
			throw new TypeError('fetch failed');
		}) as unknown as FetchFn;

		const res = await clientWith(fetch).get('/auth/me');
		expect(res.ok).toBe(false);
		if (!res.ok) {
			expect(res.error.kind).toBe('transport');
			expect(res.error.status).toBe(0);
		}
	});

	it('parses a successful json body', async () => {
		const fetch = vi.fn(async () =>
			json(200, { account: { email: 'owner@acme.test' } })
		) as unknown as FetchFn;

		const res = await clientWith(fetch).get<{ account: { email: string } }>('/auth/me');
		expect(res.ok).toBe(true);
		if (res.ok) expect(res.data.account.email).toBe('owner@acme.test');
	});
});

describe('the request it builds', () => {
	it('sends the bearer token only when it has one', async () => {
		const fetch = vi.fn(async () => json(200, {})) as unknown as FetchFn;

		await clientWith(fetch, 'sess_abc').get('/auth/me');
		const withToken = vi.mocked(fetch).mock.calls[0][1] as RequestInit;
		expect((withToken.headers as Record<string, string>).authorization).toBe('Bearer sess_abc');

		await clientWith(fetch).post('/auth/login', { email: 'a@b.test' });
		const withoutToken = vi.mocked(fetch).mock.calls[1][1] as RequestInit;
		expect((withoutToken.headers as Record<string, string>).authorization).toBeUndefined();
	});

	it('omits content-type on a bodyless post — sending one earns a 415 from axum', async () => {
		const fetch = vi.fn(async () => new Response(null, { status: 204 })) as unknown as FetchFn;

		await clientWith(fetch, 'sess_abc').post('/auth/logout');
		const init = vi.mocked(fetch).mock.calls[0][1] as RequestInit;
		expect((init.headers as Record<string, string>)['content-type']).toBeUndefined();
		expect(init.body).toBeUndefined();
	});

	it("passes credentials: 'omit', without which SvelteKit forwards the session cookie", async () => {
		// `event.fetch` defaults to `credentials: 'same-origin'` and counts a hostname *suffix* match
		// as same-origin, so it injects the browser's cookie jar into the upstream request whenever
		// `.<apiHost>`.endsWith(`.<webHost>`) — true for localhost→localhost in dev and
		// example.com→api.example.com in production. It does this *after* the headers below are built,
		// so no assertion on `headers` can see it. Verified against a live dev server: without this,
		// the API received `cookie: bf_session=...` on every single call. The API never reads cookies,
		// so it was not an auth hole — but invariant 20 says the API stays cookie-free, and it did not.
		const fetch = vi.fn(async () => json(200, {})) as unknown as FetchFn;

		await clientWith(fetch, 'sess_abc').get('/auth/me');
		const init = vi.mocked(fetch).mock.calls[0][1] as RequestInit;
		expect(init.credentials).toBe('omit');
	});
});
