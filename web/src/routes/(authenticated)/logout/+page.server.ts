import { redirect } from '@sveltejs/kit';
import type { Actions } from './$types';
import { api } from '$lib/server/api';
import * as authApi from '$lib/server/api/auth';
import { clearSessionCookie, getSessionToken } from '$lib/server/auth/cookies';

// An action-only route: there is no +page.svelte, because there is nothing to look at.
export const actions: Actions = {
	default: async (event) => {
		const token = getSessionToken(event.cookies);

		// Best effort: revoke the session row so the token is dead everywhere, not just in this browser.
		if (token) await authApi.logout(api(event, token));

		// Always clear the cookie, even if the API call failed. Being unable to reach the API is not a
		// reason to keep someone logged in against their wishes, and logout is idempotent.
		clearSessionCookie(event.cookies);

		redirect(303, '/login');
	}
};
