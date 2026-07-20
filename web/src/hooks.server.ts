import { building } from '$app/environment';
import type { Handle } from '@sveltejs/kit';
import { api } from '$lib/server/api';
import * as authApi from '$lib/server/api/auth';
import { clearSessionCookie, getSessionToken } from '$lib/server/auth/cookies';
import { assertRuntimeEnv } from '$lib/server/env';

// **Configuration is checked here, at module load — which is server start.**
//
// Every getter in `env.ts` is lazy, so before this a deployment with no `API_BASE_URL` would start
// happily, bind its port, pass a TCP healthcheck, and then 500 every page. An orchestrator calls
// that healthy. A process that refuses to start is a deploy that visibly fails, and the difference
// matters most for `ORIGIN`, whose absence 403s every write behind a TLS-terminating proxy.
//
// `!building` because `vite build` also evaluates this module, and CI has no `.env` — which is the
// whole reason those getters are lazy in the first place.
if (!building) assertRuntimeEnv();

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
