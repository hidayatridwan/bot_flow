import { redirect } from '@sveltejs/kit';
import { setError, superValidate } from 'sveltekit-superforms';
import { zod4 } from 'sveltekit-superforms/adapters';
import type { Actions, PageServerLoad } from './$types';
import { api } from '$lib/server/api';
import * as authApi from '$lib/server/api/auth';
import { setApiKeyFlash, setSessionCookie } from '$lib/server/auth/cookies';
import { mapRegisterError, toFailStatus } from '$lib/features/auth/error-map';
import { signupSchema } from '$lib/features/auth/schema';

export const load: PageServerLoad = async () => {
	return { form: await superValidate(zod4(signupSchema)) };
};

export const actions: Actions = {
	default: async (event) => {
		const form = await superValidate(event, zod4(signupSchema));

		const { name, slug, email, password } = form.data;

		// Never echo either password back — see the login action.
		form.data.password = '';
		form.data.confirmPassword = '';

		if (!form.valid) return setError(form, 'Check the fields below.', { status: 400 });

		const res = await authApi.register(api(event), {
			email: email.trim(),
			password,
			tenant_name: name,
			slug
			// confirmPassword is a client-only concept: RegisterRequest has no such field, and there is
			// no reason to put a secret on the wire twice.
		});

		if (!res.ok) {
			const { field, message } = mapRegisterError(res.error);
			const status = toFailStatus(res.error);
			// The two 409s land on different inputs — see error-map.ts.
			return field
				? setError(form, field, message, { status })
				: setError(form, message, { status });
		}

		setSessionCookie(event.cookies, res.data.session_token);

		// The one-time `sk_` reveal. Only its hash is stored server-side, so if we drop it here the
		// tenant can never see it again — they would have to mint a new one. It travels in an httpOnly
		// cookie rather than a query param, which would land in browser history and every access log.
		setApiKeyFlash(event.cookies, res.data.api_key);

		redirect(303, '/onboarding/api-key');
	}
};
