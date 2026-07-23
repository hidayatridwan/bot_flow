import { describe, expect, it } from 'vitest';
import type { ApiError } from '$lib/types/api';
import { RATE_LIMITED } from '$lib/utils/api-copy';
import { mapKeyError } from './error-map';

const error = (status: number, message: string, kind: ApiError['kind'] = 'client'): ApiError => ({
	status,
	message,
	kind,
	path: '/auth/keys'
});

describe('mapKeyError', () => {
	it('names the rate limit instead of inviting a retry', () => {
		// Minting has been metered since phase 15. Without a 429 branch this fell through to
		// "Something went wrong. Please try again" — advice that invites exactly the retry keeping the
		// caller limited, and the one message guaranteed not to help.
		expect(mapKeyError(error(429, 'rate limit exceeded, slow down'))).toBe(RATE_LIMITED);
	});

	it('passes through the API 422, which describes the caller input', () => {
		// `checked_origins` names the offending origin. That is about what the tenant typed, not about
		// our internals, so it is theirs to see (invariant 16's dividing line).
		const msg = 'origin "htp://acme.com" is not a valid origin';
		expect(mapKeyError(error(422, msg))).toBe(msg);
	});

	it('never leaks a 5xx message', () => {
		const leaky = 'connection refused: postgres://app_user@10.0.0.4:5432';
		expect(mapKeyError(error(500, leaky, 'server'))).not.toContain('postgres');
	});
});
