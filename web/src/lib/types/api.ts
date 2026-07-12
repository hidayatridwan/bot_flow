/**
 * The result of an API call. A Result, not a throw: form actions branch on `ok` and TypeScript
 * narrows `data` on the happy path, so no action needs a try/catch.
 */

export type HttpMethod = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';

export type ApiErrorKind =
	/** 4xx carrying the API's own `{"error": "..."}` body — safe to map onto a field. */
	| 'client'
	/** 5xx. */
	| 'server'
	/** The API could not be reached at all: DNS, refused, timeout. Status is 0. */
	| 'transport'
	/**
	 * A body we could not make sense of — including axum's own extractor rejections, which come back
	 * as text/plain rather than our JSON envelope. Never show `message` to a user: it leaks internals.
	 */
	| 'malformed';

export interface ApiError {
	/** 0 for transport failures — there was no response. */
	readonly status: number;
	readonly message: string;
	readonly kind: ApiErrorKind;
	/** For logs only. */
	readonly path: string;
}

export type ApiResult<T> =
	| { readonly ok: true; readonly status: number; readonly data: T }
	| { readonly ok: false; readonly error: ApiError };
