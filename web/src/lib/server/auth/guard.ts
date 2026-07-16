import { redirect } from '@sveltejs/kit';
import type { SessionUser } from '$lib/types/auth';

/**
 * The guard for the authenticated route group. Returns the user, or redirects to /login carrying
 * where the visitor was trying to go.
 */
export function requireUser(locals: App.Locals, url: URL): SessionUser {
	if (!locals.user) {
		const target = url.pathname + url.search;
		const query = target === '/dashboard' ? '' : `?redirectTo=${encodeURIComponent(target)}`;
		redirect(303, `/login${query}`);
	}
	return locals.user;
}

/**
 * As `requireUser`, but also yields the session token for `load`s that must call the API on the
 * visitor's behalf.
 *
 * The hooks set `locals.user` and `locals.session` together, but the types do not say so — this
 * narrows both in one place so no caller reaches for `locals.session!.token`. The token it returns
 * is for server-side use only: it must never appear in a `load`'s return value (invariant 20).
 */
export function requireSession(locals: App.Locals, url: URL): { user: SessionUser; token: string } {
	const user = requireUser(locals, url);
	if (!locals.session) {
		// Unreachable: hooks.server.ts sets both or neither. Redirect rather than throw — if the
		// invariant ever breaks, a visitor should see a login page, not a 500.
		redirect(303, '/login');
	}
	return { user, token: locals.session.token };
}
