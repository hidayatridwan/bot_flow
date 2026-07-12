import { env } from '$env/dynamic/private';

/**
 * `dynamic`, not `static`: adapter-node builds one artifact that must be re-pointable at a different
 * API without a rebuild.
 *
 * These are functions, not top-level consts, on purpose. A top-level `required('API_BASE_URL')` is
 * evaluated while Vite walks the module graph during `build`, so it would fail any CI that has no
 * `.env` — despite nothing actually calling the API at build time.
 */

function required(key: string): string {
	const value = env[key];
	if (!value)
		throw new Error(`Missing required env var ${key}. Copy web/.env.example to web/.env.`);
	return value;
}

export const apiBaseUrl = (): string => required('API_BASE_URL').replace(/\/+$/, '');

/** Mirrors the API's SESSION_TTL_SECS. Only sets the cookie max-age; the session row is the authority. */
export const sessionTtlSeconds = (): number => Number(env.SESSION_TTL_SECS ?? 2_592_000);

export const apiTimeoutMs = (): number => Number(env.API_TIMEOUT_MS ?? 10_000);
