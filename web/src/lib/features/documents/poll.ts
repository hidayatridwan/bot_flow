/**
 * The polling schedule. Pure and framework-free on purpose: the component owns the effect and the
 * timer, this owns the arithmetic, and so the arithmetic is unit-testable without a Svelte runtime.
 */

/** Fast enough to feel live while a small file indexes — that is when someone is watching. */
export const POLL_MIN_MS = 3_000;

/**
 * The ceiling exists because of the slow path, not the fast one: an `uploading` row whose PUT never
 * arrives sits for the presign TTL (15 min) plus UPLOAD_GRACE (5 min) before the reaper settles it.
 * At a flat 3s that is ~400 requests returning an identical body.
 *
 * That body is now one bounded page rather than the tenant's whole table, so each request costs far
 * less than it did — but the ceiling is not therefore removable. ~400 pointless round trips is still
 * ~400 round trips, and each one costs *two* API calls (the session resolve, then the list). The
 * back-off was never really about response size; pagination just narrowed the blast radius.
 */
export const POLL_MAX_MS = 15_000;

const FACTOR = 1.5;

/**
 * The next delay: back off while nothing changes, snap back to responsive the moment something does.
 *
 * 3 → 4.5 → 6.75 → 10.1 → 15 → 15… so a live upload gets several fast polls when it matters, and a
 * stalled one settles to the ceiling within ~25 seconds.
 */
export function nextInterval(currentMs: number, changed: boolean): number {
	if (changed) return POLL_MIN_MS;
	return Math.min(POLL_MAX_MS, Math.max(POLL_MIN_MS, Math.round(currentMs * FACTOR)));
}
