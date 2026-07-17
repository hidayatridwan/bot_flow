/**
 * Is this the shape of a UUID the API would accept in a path or a body?
 *
 * Shared rather than re-typed per route, for the reason the embedding client is shared in Rust: two
 * copies of a validator drift, and a regex is exactly the kind of thing that gets "tidied" in one
 * place only. Both BFF routes that relay a caller-supplied id use this one.
 *
 * It guards the *relay*, not the database: an id that reaches the API is checked there against RLS,
 * and a foreign-but-well-formed id still 404s (invariant 8). What this stops is a route interpolating
 * unvalidated text into an upstream path, and junk being forwarded for the API to reject on our
 * behalf.
 */
export const isUuid = (s: unknown): s is string =>
	typeof s === 'string' &&
	/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(s);
