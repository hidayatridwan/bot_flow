/**
 * `?redirectTo=` is attacker-controllable. Without this guard, `?redirectTo=https://evil.example`
 * turns the login page into a phishing launcher: the user logs in on our real domain and is then
 * bounced somewhere else.
 *
 * Only same-origin, absolute-path targets survive.
 */
export function safeRedirectTo(raw: string | null | undefined, fallback = '/dashboard'): string {
	if (!raw) return fallback;
	if (!raw.startsWith('/')) return fallback; // an absolute URL
	if (raw.startsWith('//') || raw.startsWith('/\\')) return fallback; // protocol-relative
	return raw;
}
