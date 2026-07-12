/**
 * The identity, and only the identity. The session token is the *credential* and lives in
 * `locals.session` under `$lib/server` — deliberately not a field here, so that a `return { user }`
 * from a load function can never put it on the wire.
 */
export interface SessionUser {
	readonly email: string;
	/** The tenant slug. */
	readonly tenantId: string;
	readonly tenantName: string;
}
