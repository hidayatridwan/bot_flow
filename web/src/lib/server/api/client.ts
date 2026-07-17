import type { ApiResult, HttpMethod } from '$lib/types/api';
import { parseResponse } from './parse';

export type FetchFn = typeof globalThis.fetch;

export interface ApiClientOptions {
	/** Without a trailing slash. */
	baseUrl: string;
	/** SvelteKit's `event.fetch`. Injected rather than imported so this module is unit-testable. */
	fetch: FetchFn;
	/** A `sess_` session token or an `sk_`/`pk_` API key. Sent as `Authorization: Bearer`. */
	token?: string;
	timeoutMs?: number;
}

export interface ApiClient {
	request<T>(method: HttpMethod, path: `/${string}`, body?: unknown): Promise<ApiResult<T>>;
	get<T>(path: `/${string}`): Promise<ApiResult<T>>;
	post<T>(path: `/${string}`, body?: unknown): Promise<ApiResult<T>>;
	del<T>(path: `/${string}`): Promise<ApiResult<T>>;
}

const DEFAULT_TIMEOUT_MS = 10_000;

export function createApiClient(opts: ApiClientOptions): ApiClient {
	const { baseUrl, fetch, token, timeoutMs = DEFAULT_TIMEOUT_MS } = opts;

	async function request<T>(
		method: HttpMethod,
		path: `/${string}`,
		body?: unknown
	): Promise<ApiResult<T>> {
		const headers: Record<string, string> = { accept: 'application/json' };
		// Only when there is a body: sending a content-type on a bodyless POST earns a 415 from axum.
		if (body !== undefined) headers['content-type'] = 'application/json';
		if (token) headers['authorization'] = `Bearer ${token}`;

		let res: Response;
		try {
			res = await fetch(`${baseUrl}${path}`, {
				method,
				headers,
				body: body === undefined ? undefined : JSON.stringify(body),
				// Keeps invariant 20's "the API stays cookie-free" true, which it was not.
				// `event.fetch` defaults to `credentials: 'same-origin'` and treats a *hostname suffix*
				// match as same-origin — so it injects the browser's cookie jar into the upstream
				// request whenever `.<apiHost>`.endsWith(`.<webHost>`). That holds for
				// localhost→localhost in dev and example.com→api.example.com in production, and it
				// happens after this header object is built, so nothing here could have revealed it.
				// The API never reads cookies, so this was not an auth hole — but it shipped
				// `bf_session` to the API on every call, where it lands in access logs, and invariant
				// 18's CORS reasoning is supposed to rest on the API never seeing a cookie at all.
				credentials: 'omit',
				// A hung API must not hang server-side rendering.
				signal: AbortSignal.timeout(timeoutMs)
			});
		} catch (cause) {
			console.error(`[api] ${method} ${path} could not be reached:`, cause);
			return {
				ok: false,
				error: { status: 0, message: 'could not reach the api', kind: 'transport', path }
			};
		}

		const result = await parseResponse<T>(res, path);
		if (!result.ok && result.error.kind !== 'client') {
			// 'server' and 'malformed' are our bugs, not the caller's. Log them in full; the UI will
			// only ever show a generic message.
			console.error(`[api] ${method} ${path} → ${result.error.status}: ${result.error.message}`);
		}
		return result;
	}

	return {
		request,
		get: (path) => request('GET', path),
		post: (path, body) => request('POST', path, body),
		del: (path) => request('DELETE', path)
	};
}
