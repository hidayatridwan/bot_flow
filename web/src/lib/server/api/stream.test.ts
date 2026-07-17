import { describe, expect, it, vi } from 'vitest';
import { createStreamClient } from './stream';

/** A Response whose body is a real ReadableStream, as the API's would be. */
function sseResponse(text: string, init: ResponseInit = {}) {
	const body = new ReadableStream<Uint8Array>({
		start(c) {
			c.enqueue(new TextEncoder().encode(text));
			c.close();
		}
	});
	return new Response(body, {
		status: 200,
		headers: { 'content-type': 'text/event-stream' },
		...init
	});
}

const client = (fetch: typeof globalThis.fetch, token = 'sess_abc') =>
	createStreamClient({ baseUrl: 'http://api:3000', fetch, token, timeoutMs: 1000 });

async function drain(body: ReadableStream<Uint8Array>) {
	let out = '';
	const reader = body.getReader();
	const decoder = new TextDecoder();
	for (;;) {
		const { value, done } = await reader.read();
		if (done) break;
		out += decoder.decode(value, { stream: true });
	}
	return out;
}

describe('createStreamClient', () => {
	describe('the header contract', () => {
		// The assertions that matter most here. A leak would still stream perfectly, and nothing else
		// in the system would notice — same reason `upload.test.ts` pins its own headers.

		it('sends only authorization, content-type and accept', async () => {
			const fetch = vi.fn().mockResolvedValue(sseResponse('event: done\n\n'));
			await client(fetch).post('/ask/stream', { query: 'hi' });

			const headers = fetch.mock.calls[0][1].headers;
			expect(Object.keys(headers).sort()).toEqual(['accept', 'authorization', 'content-type']);
			expect(headers.authorization).toBe('Bearer sess_abc');
			expect(headers.accept).toBe('text/event-stream');
		});

		it('builds no cookie or origin header of its own', async () => {
			const fetch = vi.fn().mockResolvedValue(sseResponse('event: done\n\n'));
			await client(fetch).post('/ask/stream', { query: 'hi' });

			const headers = fetch.mock.calls[0][1].headers;
			expect(headers).not.toHaveProperty('cookie');
			expect(headers).not.toHaveProperty('origin');
		});

		it("passes credentials: 'omit', without which SvelteKit forwards the session cookie", async () => {
			// The header object above is NOT the last word: `event.fetch` appends to it afterwards.
			// It defaults to `credentials: 'same-origin'` and counts a hostname *suffix* match as
			// same-origin, so it injects the browser's cookie jar whenever
			// `.<apiHost>`.endsWith(`.<webHost>`) — localhost→localhost in dev,
			// example.com→api.example.com in production. Verified against a live dev server: without
			// this, the API receives `cookie: bf_session=...` on every call, and invariant 20's "the
			// API stays cookie-free" is simply untrue. No assertion on `headers` can catch it.
			const fetch = vi.fn().mockResolvedValue(sseResponse('event: done\n\n'));
			await client(fetch).post('/ask/stream', { query: 'hi' });

			expect(fetch.mock.calls[0][1].credentials).toBe('omit');
		});

		it('omits authorization entirely when there is no token', async () => {
			const fetch = vi.fn().mockResolvedValue(sseResponse('event: done\n\n'));
			await createStreamClient({
				baseUrl: 'http://api:3000',
				fetch,
				timeoutMs: 1000
			}).post('/ask/stream', { query: 'hi' });

			expect(fetch.mock.calls[0][1].headers).not.toHaveProperty('authorization');
		});
	});

	it('hands the body back unread', async () => {
		// The whole point: the stream reaches the caller intact. `client.ts` cannot do this — its
		// `parseResponse` would have consumed it.
		const fetch = vi
			.fn()
			.mockResolvedValue(sseResponse('event: token\ndata: hi\n\nevent: done\n\n'));
		const res = await client(fetch).post('/ask/stream', { query: 'hi' });

		expect(res.ok).toBe(true);
		if (!res.ok) return;
		expect(await drain(res.body)).toBe('event: token\ndata: hi\n\nevent: done\n\n');
	});

	it('posts the body as json to the right url', async () => {
		const fetch = vi.fn().mockResolvedValue(sseResponse('event: done\n\n'));
		await client(fetch).post('/ask/stream', { query: 'hi', conversation_id: '' });

		expect(fetch.mock.calls[0][0]).toBe('http://api:3000/ask/stream');
		expect(fetch.mock.calls[0][1].method).toBe('POST');
		expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual({ query: 'hi', conversation_id: '' });
	});

	describe('the error path', () => {
		// A non-2xx never streams: auth, rate limiting, the rewrite and retrieval all run before the
		// SSE response exists. So these arrive as ordinary short bodies, and `parseResponse` — not a
		// second copy of it — is what reads them.

		it('reads our json envelope, keeping the status and kind', async () => {
			const fetch = vi.fn().mockResolvedValue(
				new Response(JSON.stringify({ error: 'rate limit exceeded' }), {
					status: 429,
					headers: { 'content-type': 'application/json' }
				})
			);
			const res = await client(fetch).post('/ask/stream', { query: 'hi' });

			expect(res).toEqual({
				ok: false,
				error: { status: 429, message: 'rate limit exceeded', kind: 'client', path: '/ask/stream' }
			});
		});

		it('marks a 5xx as server, not client', async () => {
			const fetch = vi.fn().mockResolvedValue(
				new Response(JSON.stringify({ error: 'internal server error' }), {
					status: 500,
					headers: { 'content-type': 'application/json' }
				})
			);
			const res = await client(fetch).post('/ask/stream', { query: 'hi' });
			expect(res.ok).toBe(false);
			if (res.ok) return;
			// `kind` is what lets a caller tell "you did something wrong" from "we are broken" —
			// invariant 21 rests on that distinction.
			expect(res.error.kind).toBe('server');
		});

		it('handles an axum extractor rejection, which is text/plain and not our envelope', async () => {
			const fetch = vi.fn().mockResolvedValue(
				new Response('Failed to deserialize the JSON body into the target type', {
					status: 422,
					headers: { 'content-type': 'text/plain; charset=utf-8' }
				})
			);
			const res = await client(fetch).post('/ask/stream', { query: 'hi' });
			expect(res.ok).toBe(false);
			if (res.ok) return;
			expect(res.error.status).toBe(422);
			// `malformed`, so the UI shows generic copy rather than axum's internals (invariant 16).
			expect(res.error.kind).toBe('malformed');
		});

		it('does not call a bodyless non-2xx a success', async () => {
			// `parseResponse` returns ok for an empty body *before* it checks the status. Our API always
			// sends an envelope; a proxy's bare 502 does not. Reporting that as ok would hand the
			// caller an undefined body.
			const fetch = vi
				.fn()
				.mockResolvedValue(new Response(null, { status: 502, headers: { 'content-length': '0' } }));
			const res = await client(fetch).post('/ask/stream', { query: 'hi' });

			expect(res.ok).toBe(false);
			if (res.ok) return;
			expect(res.error.status).toBe(502);
			expect(res.error.kind).toBe('server');
		});
	});

	it('reports an unreachable api as transport, not as a crash', async () => {
		// `kind: 'transport'` with status 0 is what invariant 21 keys on: an API outage must render as
		// logged-out-for-this-request, never as a logout.
		const fetch = vi.fn().mockRejectedValue(new TypeError('fetch failed'));
		const res = await client(fetch).post('/ask/stream', { query: 'hi' });

		expect(res).toEqual({
			ok: false,
			error: {
				status: 0,
				message: 'could not reach the api',
				kind: 'transport',
				path: '/ask/stream'
			}
		});
	});

	it('passes a signal that a timeout can fire', async () => {
		const fetch = vi.fn().mockResolvedValue(sseResponse('event: done\n\n'));
		await client(fetch).post('/ask/stream', { query: 'hi' });
		expect(fetch.mock.calls[0][1].signal).toBeInstanceOf(AbortSignal);
	});

	it('aborts when the caller-supplied signal fires, not only on its own deadline', async () => {
		// The inbound request's signal is combined with the deadline. It does not fire today — Kit's
		// getRequest skips the abort once the request body has fully arrived — but the wiring must be
		// right for the day it does.
		const inbound = new AbortController();
		const fetch = vi.fn().mockImplementation((_url, init) => {
			return new Promise((_resolve, reject) => {
				init.signal.addEventListener('abort', () => reject(new Error('aborted')));
			});
		});
		const promise = createStreamClient({
			baseUrl: 'http://api:3000',
			fetch,
			timeoutMs: 60_000, // long enough that only the inbound signal can end this
			signal: inbound.signal
		}).post('/ask/stream', { query: 'hi' });

		inbound.abort();
		const res = await promise;
		expect(res.ok).toBe(false);
		if (res.ok) return;
		expect(res.error.kind).toBe('transport');
	});

	it('gives up when the deadline passes', async () => {
		const fetch = vi.fn().mockImplementation((_url, init) => {
			return new Promise((_resolve, reject) => {
				init.signal.addEventListener('abort', () => reject(new Error('timed out')));
			});
		});
		const res = await createStreamClient({
			baseUrl: 'http://api:3000',
			fetch,
			timeoutMs: 10
		}).post('/ask/stream', { query: 'hi' });

		expect(res.ok).toBe(false);
		if (res.ok) return;
		expect(res.error.kind).toBe('transport');
	});
});
