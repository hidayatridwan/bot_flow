import { redirect } from '@sveltejs/kit';
import { setError, superValidate } from 'sveltekit-superforms';
import { zod4 } from 'sveltekit-superforms/adapters';
import type { Actions, PageServerLoad } from './$types';
import { api } from '$lib/server/api';
import * as authApi from '$lib/server/api/auth';
import { mapResetPasswordError, toFailStatus } from '$lib/features/auth/error-map';
import { resetPasswordSchema } from '$lib/features/auth/schema';

export const load: PageServerLoad = async (event) => {
	// The token arrives in the query string because that is the only place a link can carry one, and
	// it is moved straight into the form as a hidden field. It is **not** validated here: this page
	// cannot tell a fake token from an expired one, only the API can, and pre-judging it would mean
	// showing "invalid link" to someone whose link is fine but whose clock we misread.
	const token = event.url.searchParams.get('token') ?? '';
	const form = await superValidate(zod4(resetPasswordSchema));
	form.data.token = token;

	// `hasToken` rather than an early `error()`: someone who lands here bare (a bookmark, a truncated
	// link) needs a way forward, not a 400 page.
	return { form, hasToken: token.length > 0 };
};

export const actions: Actions = {
	default: async (event) => {
		const form = await superValidate(event, zod4(resetPasswordSchema));

		const token = form.data.token;
		const password = form.data.password;

		// Same rule as login and signup: never let a secret survive into the SSR html. The token is a
		// credential too, and it is the one that would still be live after a failed attempt.
		form.data.password = '';
		form.data.confirmPassword = '';

		if (!form.valid) return setError(form, 'Check the fields below.', { status: 400 });

		const res = await authApi.resetPassword(api(event), { token, password });

		if (!res.ok) {
			const { field, message } = mapResetPasswordError(res.error);
			return field
				? setError(form, field, message, { status: toFailStatus(res.error) })
				: setError(form, message, { status: toFailStatus(res.error) });
		}

		// No session is set, because the API deliberately issues none: redeeming a link proves control
		// of an inbox, not knowledge of the password. The user has just chosen one — making them use
		// it closes the gap where a leaked link becomes a live session without a password being typed.
		redirect(303, '/login?reset=1');
	}
};
