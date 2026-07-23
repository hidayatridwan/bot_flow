import type { ApiError } from '$lib/types/api';
import { genericMessage, RATE_LIMITED } from '$lib/utils/api-copy';

/**
 * `/auth/keys*` errors → copy. The 422s are the interesting ones: they carry the API's own origin
 * diagnostics, which are safe to show because they describe *the caller's input*, not our internals
 * (invariant 16's dividing line).
 */
export function mapKeyError(error: ApiError): string {
	// Minting has been metered since phase 15 (its own `keys:{tenant}` bucket). Without this branch a
	// 429 fell through to "Something went wrong. Please try again" — advice that invites exactly the
	// retry that keeps the caller limited, and the one piece of copy guaranteed not to help.
	if (error.status === 429) return RATE_LIMITED;

	if (error.kind === 'client') {
		// 422 from `checked_origins` — it names the offending origin, or says the allow-list is empty.
		// Both are about what the tenant typed, so both are theirs to see.
		if (error.status === 422) return error.message;
		if (error.status === 404) return 'That key no longer exists. Reload the page.';
		if (error.status === 400) return error.message;
	}
	return genericMessage(error);
}
