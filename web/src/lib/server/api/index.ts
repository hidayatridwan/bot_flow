import type { RequestEvent } from '@sveltejs/kit';
import { apiBaseUrl, apiTimeoutMs } from '$lib/server/env';
import { createApiClient, type ApiClient } from './client';

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

export type { ApiClient } from './client';
