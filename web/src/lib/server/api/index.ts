import type { RequestEvent } from '@sveltejs/kit';
import { apiBaseUrl, apiTimeoutMs, askTimeoutMs } from '$lib/server/env';
import { createApiClient, type ApiClient } from './client';
import { createStreamClient, type StreamClient } from './stream';

/**
 * The only place env is read. The token is passed explicitly rather than pulled from `locals`, which
 * is why `handleFetch` is not used: a hook that attached the Bearer token would attach it to *every*
 * outbound fetch, third-party URLs included.
 */
export function api(event: RequestEvent, token?: string): ApiClient {
	return createApiClient({
		baseUrl: apiBaseUrl(),
		fetch: event.fetch,
		token,
		timeoutMs: apiTimeoutMs()
	});
}

/**
 * The same, for a response we pipe instead of parse. Separate because `ApiClient` cannot carry a
 * stream at all — see `stream.ts` — and because an ask needs its own, much longer budget.
 */
export function apiStream(event: RequestEvent, token?: string): StreamClient {
	return createStreamClient({
		baseUrl: apiBaseUrl(),
		fetch: event.fetch,
		token,
		timeoutMs: askTimeoutMs(),
		signal: event.request.signal
	});
}

export type { ApiClient } from './client';
export type { StreamClient, StreamResult } from './stream';
