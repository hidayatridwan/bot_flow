import type { ApiError } from '$lib/types/api';
import type { FetchFn } from './client';
import { parseResponse } from './parse';

/**
 * Fetching a response we hand on as a *stream* rather than read as a value.
 *
 * **Why this is not `client.ts` with a flag.** Two independent blocks, either one fatal:
 *
 * 1. `client.ts` returns `ApiResult<T>` — a value — and gets there via `parseResponse`, whose very
 *    first move on any non-204 is `await res.text()`. It consumes the body by construction. There is
 *    no flag that un-consumes it.
 * 2. Its `AbortSignal.timeout(10_000)` covers the **body**, not just the headers. Every answer longer
 *    than ten seconds would be cut mid-sentence.
 *
 * So this is a sibling, not a fork. What it does *not* duplicate is the interesting part: the error
 * path calls straight back into `parseResponse`, because a non-2xx from `/ask/stream` is a short,
 * complete body like any other — auth, rate limiting, the query rewrite and retrieval all run before
 * the SSE response is constructed, so a failure there arrives as an ordinary HTTP error and never
 * mid-stream. Only the *success* path is special, and it is three lines.
 *
 * The body is handed on untouched. Nothing here parses SSE: that is `features/chat/sse.ts`, in the
 * browser. Keeping this a dumb pipe is what makes the proxy cheap — and it is also why the API, not
 * this file, is where an `error` frame's detail must be scrubbed (invariant 16): we never look inside.
 */

export type StreamResult =
	{ ok: true; status: number; body: ReadableStream<Uint8Array> } | { ok: false; error: ApiError };

export interface StreamClientOptions {
	/** Without a trailing slash. */
	baseUrl: string;
	/** Injected, as in `client.ts`, so the header contract below is assertable in a unit test. */
	fetch: FetchFn;
	/** A `sess_` token. Read from `locals` on the server; it never reaches the browser (invariant 20). */
	token?: string;
	/** Headers *and* body. See `askTimeoutMs` for why this is not the ordinary API budget. */
	timeoutMs: number;
	/** The inbound request's signal, if the caller has one. See `post` for what it is worth today. */
	signal?: AbortSignal;
}

export interface StreamClient {
	post(path: `/${string}`, body: unknown): Promise<StreamResult>;
}

const error = (
	status: number,
	message: string,
	kind: ApiError['kind'],
	path: string
): ApiError => ({
	status,
	message,
	kind,
	path
});

export function createStreamClient(opts: StreamClientOptions): StreamClient {
	const { baseUrl, fetch, token, timeoutMs, signal } = opts;

	async function post(path: `/${string}`, body: unknown): Promise<StreamResult> {
		// Built from nothing, never spread from the inbound request: three headers, listed.
		const headers: Record<string, string> = {
			'content-type': 'application/json',
			accept: 'text/event-stream'
		};
		if (token) headers.authorization = `Bearer ${token}`;

		// `signal` is included because it is free and correct, not because it works: SvelteKit's
		// `getRequest` only aborts when `(errored || request.destroyed) && !end_emitted`, and a POST
		// whose body arrived in full has `end_emitted`. So a browser that closes the tab mid-answer
		// does *not* cancel this today, and the deadline is the only bound that actually fires. The
		// upstream keeps draining until the LLM finishes — bounded by `max_tokens`, and harmless:
		// `append_turn` still runs, so history stays a faithful record.
		const deadline = AbortSignal.timeout(timeoutMs);
		const abort = signal ? AbortSignal.any([signal, deadline]) : deadline;

		let res: Response;
		try {
			res = await fetch(`${baseUrl}${path}`, {
				method: 'POST',
				headers,
				body: JSON.stringify(body),
				// Not decoration, and not the default. `event.fetch` defaults `credentials` to
				// `same-origin`, and its idea of same-origin is a *hostname suffix* match: it injects
				// the browser's cookie jar into the upstream request whenever
				// `.<apiHost>`.endsWith(`.<webHost>`). That is true for localhost→localhost in dev and
				// for example.com→api.example.com in production, so the header object above is not the
				// last word on what gets sent — Kit appends to it after this returns. Without `omit`,
				// `bf_session` reaches the API on every call, which is exactly what invariant 20 says
				// does not happen. The API ignores cookies, so this was never an auth hole; it put the
				// session token in the API's logs and made the invariant's stated mechanism a fiction.
				credentials: 'omit',
				signal: abort
			});
		} catch (cause) {
			console.error(`[api] POST ${path} could not be reached:`, cause);
			return { ok: false, error: error(0, 'could not reach the api', 'transport', path) };
		}

		if (!res.ok) {
			const parsed = await parseResponse<unknown>(res, path);
			if (!parsed.ok) return { ok: false, error: parsed.error };
			// `parseResponse` answers `ok` for an empty body *before* it looks at the status, so a
			// bodyless non-2xx lands here. Our API always sends an envelope, but a proxy's bare 502
			// would not — and reporting that as success would hand the caller an undefined body.
			const kind = res.status >= 500 ? 'server' : 'client';
			return { ok: false, error: error(res.status, 'the api sent an empty error', kind, path) };
		}

		if (!res.body) {
			return { ok: false, error: error(res.status, 'the api sent no body', 'malformed', path) };
		}

		return { ok: true, status: res.status, body: res.body };
	}

	return { post };
}
