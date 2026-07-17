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

/**
 * The stream completed cleanly, found passages, and produced no words.
 *
 * Not hypothetical, and not the same as a refusal. A refusal has *no* sources and a canned sentence —
 * the system declining to guess (invariant 4). This is the opposite: retrieval succeeded, so the bot
 * had something to say and said nothing.
 *
 * The cause lives in the model. Every reasoning model bills its thinking against the same
 * `max_tokens` budget as its prose, so a question that thinks hard enough can spend the whole budget
 * and emit no `content` at all — `finish_reason: "length"`, zero content deltas, and an API that
 * quite correctly yields `done` because nothing failed. Reproduced against the configured gateway.
 *
 * Retrying is genuinely the right advice: the budget is per-request and thinking is not deterministic.
 */
export const NO_WORDS =
	"The bot found relevant passages but didn't produce an answer. Try asking again.";

export function mapAskError(error: ApiError): string {
	if (error.status === 429) return RATE_LIMITED;
	if (error.status === 401) return SESSION_EXPIRED;

	// A conversation id the API will not accept — unknown, or another tenant's. Both 404 identically
	// (invariant 8), and this copy must not distinguish them either: "no such conversation" and "not
	// yours" are the same sentence here, or the UI re-opens the oracle the API closed.
	if (error.status === 404) return 'That conversation has ended. Ask again to start a new one.';

	return genericMessage(error);
}
