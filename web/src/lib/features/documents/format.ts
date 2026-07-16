/**
 * Rendering the API's timestamps.
 *
 * `GET /documents` sends `created_at::text` straight from Postgres, which is *not* ISO 8601:
 *
 *     2026-07-16 11:39:20.470205+00
 *              ^ a space, not a `T`   ^ a two-digit offset, where ISO wants `+00:00` or `Z`
 *
 * `new Date()` rejects both, silently, by returning `Invalid Date` — so a naive implementation shows
 * the raw Postgres string to the user and nothing errors. Pure and tested for exactly that reason.
 */
export function formatCreatedAt(createdAt: string, locale?: string): string {
	const iso = createdAt.replace(' ', 'T').replace(/([+-]\d{2})$/, '$1:00');
	const date = new Date(iso);
	// Fall back to the raw value rather than rendering "Invalid Date": if the API's format ever
	// shifts again, an ugly timestamp beats a broken one.
	return isNaN(date.getTime()) ? createdAt : date.toLocaleString(locale);
}
