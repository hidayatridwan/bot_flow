import type { ApiError } from '$lib/types/api';

/**
 * What we say when the API fails for a reason the user did not cause.
 *
 * Shared rather than per-slice: these describe the API *boundary*, not any one feature, and two
 * slices phrasing "something went wrong" two different ways is a product bug wearing a very small
 * costume. Pure and browser-safe, so a component may import it.
 */

export const GENERIC = 'Something went wrong. Please try again.';
export const UNREACHABLE = "We couldn't reach the service. Please try again in a moment.";
export const RATE_LIMITED = 'Too many attempts. Please wait a minute and try again.';

/**
 * The safe message for an error the caller cannot act on: 5xx, transport, and `malformed` (an axum
 * extractor rejection).
 *
 * Never returns `error.message` — that text describes our internals, which invariant 16 keeps away
 * from clients. The client logs the raw string; this is what a human sees.
 */
export function genericMessage(error: ApiError): string {
	return error.kind === 'transport' ? UNREACHABLE : GENERIC;
}
