import { z } from 'zod';

/**
 * The TS mirror of `auth::normalize_origin`. Drift here shows the user a client-side "valid" that the
 * API then 422s — the same trap `documents/schema.ts` documents for `extension_of`.
 *
 * The API is the authority: it re-normalises everything it stores. This exists to fail fast and to
 * show the tenant the canonical form *before* they commit to it, because an origin is compared to the
 * `Origin` header by string equality — a non-canonical entry is dead, not lax.
 */

export const KEY_KINDS = ['secret', 'publishable'] as const;
export type KeyKind = (typeof KEY_KINDS)[number];

/** Mirrors `auth::normalize_origin`. Returns the canonical origin, or null if it can never match. */
export function normalizeOrigin(raw: string): string | null {
	const trimmed = raw.trim();
	// `null` is a file:// page or a sandboxed iframe. Allow-listing it admits every such page.
	if (!trimmed || trimmed.toLowerCase() === 'null') return null;

	const sep = trimmed.indexOf('://');
	if (sep === -1) return null;
	const scheme = trimmed.slice(0, sep).toLowerCase();
	if (scheme !== 'http' && scheme !== 'https') return null;

	// A trailing `/` is forgiven — it is what a human copies from an address bar. Nothing else is.
	let rest = trimmed.slice(sep + 3);
	if (rest.endsWith('/')) rest = rest.slice(0, -1);
	if (!rest || /[/?#@ ]/.test(rest)) return null;

	let host = rest;
	let port: number | null = null;
	const colon = rest.lastIndexOf(':');
	if (colon !== -1) {
		const rawPort = rest.slice(colon + 1);
		if (!/^\d+$/.test(rawPort)) return null;
		port = Number(rawPort);
		if (port > 65535) return null;
		host = rest.slice(0, colon);
	}
	if (!host) return null;

	// The browser omits the default port, so storing it guarantees a mismatch.
	if ((scheme === 'https' && port === 443) || (scheme === 'http' && port === 80)) port = null;

	host = host.toLowerCase();
	return port === null ? `${scheme}://${host}` : `${scheme}://${host}:${port}`;
}

/** Split a textarea into origins: one per line, blanks ignored. */
export function parseOrigins(raw: string): string[] {
	return raw
		.split('\n')
		.map((l) => l.trim())
		.filter(Boolean);
}

/** The first line that cannot be an origin, or null if all of them can. */
export function firstInvalidOrigin(raw: string): string | null {
	return parseOrigins(raw).find((o) => normalizeOrigin(o) === null) ?? null;
}

export const mintSchema = z
	.object({
		kind: z.enum(KEY_KINDS),
		label: z.string().max(64, 'Keep the label under 64 characters.').default(''),
		/** A textarea, one origin per line. Empty is legal for a secret key, fatal for a publishable one. */
		origins: z.string().default('')
	})
	.refine((v) => firstInvalidOrigin(v.origins) === null, {
		path: ['origins'],
		message: 'Use scheme://host[:port] — for example https://example.com'
	})
	.refine((v) => v.kind !== 'publishable' || parseOrigins(v.origins).length > 0, {
		path: ['origins'],
		// Mirrors the API's rule. An empty allow-list is not a permissive publishable key — it is one
		// that 403s on every request, forever.
		message: 'A publishable key needs at least one origin, or it cannot answer from anywhere.'
	});

export type MintSchema = typeof mintSchema;
