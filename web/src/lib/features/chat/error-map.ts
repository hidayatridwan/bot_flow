import type { ApiError } from '$lib/types/api';
import { RATE_LIMITED, genericMessage } from '$lib/utils/api-copy';

/**
 * Ask failures → what the user is told.
 *
 * Note what is *not* here: a case for the model refusing to answer. A refusal is a successful
 * response (invariant 4) and belongs in the transcript as an ordinary reply — routing it through an
 * error map would tell the tenant their bot is broken at the exact moment it is working correctly.
 */

/** The session died mid-page. A reload lands on the guard, which redirects properly. */
export const SESSION_EXPIRED = 'Your session has expired. Please log in again.';

/**
 * The stream ended without its `done` sentinel — a timeout, a dropped socket, a killed API.
 *
 * Deliberately not "something went wrong": whatever tokens did arrive are still on screen and still
 * true, so the copy has to explain a *partial* answer rather than disown it.
 */
export const CUT_SHORT = 'The answer was cut short. Ask again to see the rest.';

export function mapAskError(error: ApiError): string {
	if (error.status === 429) return RATE_LIMITED;
	if (error.status === 401) return SESSION_EXPIRED;

	// A conversation id the API will not accept — unknown, or another tenant's. Both 404 identically
	// (invariant 8), and this copy must not distinguish them either: "no such conversation" and "not
	// yours" are the same sentence here, or the UI re-opens the oracle the API closed.
	if (error.status === 404) return 'That conversation has ended. Ask again to start a new one.';

	return genericMessage(error);
}
