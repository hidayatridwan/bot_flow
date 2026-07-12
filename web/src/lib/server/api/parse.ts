import type { ApiError, ApiResult } from '$lib/types/api';

/** The API's error envelope: `{"error": "..."}`. Anything else is not ours. */
function messageFromEnvelope(parsed: unknown): string | null {
	if (typeof parsed === 'object' && parsed !== null && 'error' in parsed) {
		const { error } = parsed as { error: unknown };
		if (typeof error === 'string') return error;
	}
	return null;
}

function err(status: number, message: string, kind: ApiError['kind'], path: string): ApiError {
	return { status, message, kind, path };
}

/**
 * Turn a Response into an ApiResult.
 *
 * The order below is the design, not a style:
 *
 * 1. A 204 (logout) has no body at all. Trying to parse one is how logout breaks.
 * 2. Read the body as *text*, once, always. `res.json()` would throw on a text/plain body and take
 *    the body with it — and axum's own extractor rejections (415/400/422 on a malformed request)
 *    are text/plain, not our JSON envelope.
 * 3. Only then decide whether it was JSON.
 */
export async function parseResponse<T>(res: Response, path: string): Promise<ApiResult<T>> {
	if (res.status === 204 || res.headers.get('content-length') === '0') {
		return { ok: true, status: res.status, data: undefined as T };
	}

	const text = await res.text();
	const isJson = (res.headers.get('content-type') ?? '').includes('application/json');

	if (res.ok) {
		if (!text) return { ok: true, status: res.status, data: undefined as T };
		if (!isJson) {
			return { ok: false, error: err(res.status, 'expected a json body', 'malformed', path) };
		}
		try {
			return { ok: true, status: res.status, data: JSON.parse(text) as T };
		} catch {
			return { ok: false, error: err(res.status, 'could not parse the body', 'malformed', path) };
		}
	}

	const kind = res.status >= 500 ? 'server' : 'client';

	if (isJson) {
		try {
			const message = messageFromEnvelope(JSON.parse(text));
			if (message !== null) return { ok: false, error: err(res.status, message, kind, path) };
		} catch {
			/* fall through to malformed */
		}
	}

	// An axum extractor rejection, or something else we do not own. `message` is for the log only:
	// surfacing it would show the user "Failed to deserialize the JSON body into the target type...".
	return { ok: false, error: err(res.status, text.slice(0, 200), 'malformed', path) };
}
