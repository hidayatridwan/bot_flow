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
