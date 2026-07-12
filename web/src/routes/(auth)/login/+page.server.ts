import { redirect } from '@sveltejs/kit';
import { setError, superValidate } from 'sveltekit-superforms';
import { zod4 } from 'sveltekit-superforms/adapters';
import type { Actions, PageServerLoad } from './$types';
import { api } from '$lib/server/api';
import * as authApi from '$lib/server/api/auth';
import { setSessionCookie } from '$lib/server/auth/cookies';
import { mapLoginError, toFailStatus } from '$lib/features/auth/error-map';
import { loginSchema } from '$lib/features/auth/schema';
import { safeRedirectTo } from '$lib/utils/redirect';

export const load: PageServerLoad = async () => {
	return { form: await superValidate(zod4(loginSchema)) };
};

export const actions: Actions = {
	default: async (event) => {
		const form = await superValidate(event, zod4(loginSchema));

		const email = form.data.email.trim();
		const password = form.data.password;

		// Never echo a password back to the browser: whatever is left on `form.data` is serialised
		// into the SSR html and visible in the network panel. The password manager refills it anyway.
		form.data.password = '';

		if (!form.valid) return setError(form, 'Check the fields below.', { status: 400 });

		const res = await authApi.login(api(event), { email, password });

		if (!res.ok) {
			const { message } = mapLoginError(res.error);
			// Always form-level. mapLoginError never returns a field, and this action must never add
			// one: the API's uniform "invalid email or password" exists so the endpoint cannot be used
			// to discover which emails are registered. Put it under Email and the UI hands that back.
			return setError(form, message, { status: toFailStatus(res.error) });
		}

		setSessionCookie(event.cookies, res.data.session_token);

		// From event.url, not a hidden input: a bare method="POST" form posts to location.href, so the
		// query string is already here. safeRedirectTo blocks an open redirect to another origin.
		redirect(303, safeRedirectTo(event.url.searchParams.get('redirectTo')));
	}
};
