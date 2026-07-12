import type { Cookies } from '@sveltejs/kit';
import { dev } from '$app/environment';
import { sessionTtlSeconds } from '$lib/server/env';

export const SESSION_COOKIE = 'bf_session';
export const API_KEY_FLASH_COOKIE = 'bf_api_key_once';

const base = {
	// The browser must never read the token. That is the entire point of the BFF: the API stays
	// cookie-free (invariant 18), and the cookie lives on *this* origin only.
	httpOnly: true,
	secure: !dev,
	// Not 'strict': that would break the /login?redirectTo=... flow arriving from an external link.
	// SvelteKit's csrf.checkOrigin already rejects cross-origin form posts.
	sameSite: 'lax',
	path: '/'
} as const;

/**
 * The cookie is only a hint. The sessions row in Postgres is the authority and GET /auth/me is the
 * check — a cookie that outlives its session costs exactly one 401.
 */
export function setSessionCookie(cookies: Cookies, token: string): void {
	cookies.set(SESSION_COOKIE, token, { ...base, maxAge: sessionTtlSeconds() });
}

export function clearSessionCookie(cookies: Cookies): void {
	cookies.delete(SESSION_COOKIE, { path: '/' });
}

export function getSessionToken(cookies: Cookies): string | null {
	return cookies.get(SESSION_COOKIE) ?? null;
}

/**
 * Carries the one-time `sk_` from the register response to the page that reveals it.
 *
 * httpOnly, so JS cannot read it. Not a URL param — that would land in browser history, in the
 * Referer of every outbound link on the page, and in every proxy access log.
 */
export function setApiKeyFlash(cookies: Cookies, apiKey: string): void {
	cookies.set(API_KEY_FLASH_COOKIE, apiKey, { ...base, maxAge: 300 });
}

/** Reads and deletes in one go, so the key is shown exactly once — as the API itself promises. */
export function takeApiKeyFlash(cookies: Cookies): string | null {
	const value = cookies.get(API_KEY_FLASH_COOKIE) ?? null;
	if (value) cookies.delete(API_KEY_FLASH_COOKIE, { path: '/' });
	return value;
}
