import type { Handle } from '@sveltejs/kit';
import { api } from '$lib/server/api';
import * as authApi from '$lib/server/api/auth';
import { clearSessionCookie, getSessionToken } from '$lib/server/auth/cookies';

export const handle: Handle = async ({ event, resolve }) => {
	event.locals.session = null;
	event.locals.user = null;

	const token = getSessionToken(event.cookies);

	// A visitor with no cookie costs zero API calls.
	if (token) {
		const res = await authApi.me(api(event, token));

		if (res.ok) {
			event.locals.session = { token };
			event.locals.user = {
				email: res.data.account.email,
				tenantId: res.data.tenant.id,
				tenantName: res.data.tenant.name
			};
		} else if (res.error.status === 401) {
			// The session is dead or expired. Drop the cookie so we stop asking on every request.
			clearSessionCookie(event.cookies);
		}
		// Any other failure (5xx, transport) leaves locals null but KEEPS the cookie: a blip in the
		// API must not silently log every user out. Once it recovers, a reload just works.
	}

	return resolve(event);
};
