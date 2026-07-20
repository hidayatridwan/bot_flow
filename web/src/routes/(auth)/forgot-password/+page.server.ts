import { setError, superValidate } from 'sveltekit-superforms';
import { zod4 } from 'sveltekit-superforms/adapters';
import type { Actions, PageServerLoad } from './$types';
import { api } from '$lib/server/api';
import * as authApi from '$lib/server/api/auth';
import { mapForgotPasswordError, toFailStatus } from '$lib/features/auth/error-map';
import { forgotPasswordSchema } from '$lib/features/auth/schema';

export const load: PageServerLoad = async () => {
	return { form: await superValidate(zod4(forgotPasswordSchema)) };
};

export const actions: Actions = {
	default: async (event) => {
		const form = await superValidate(event, zod4(forgotPasswordSchema));
		if (!form.valid) return setError(form, 'Check the fields below.', { status: 400 });

		const res = await authApi.forgotPassword(api(event), { email: form.data.email.trim() });

		if (!res.ok) {
			const { message } = mapForgotPasswordError(res.error);
			return setError(form, message, { status: toFailStatus(res.error) });
		}

		// `sent`, not a redirect, and deliberately the same for every address.
		//
		// The API answers 202 whether or not the account exists (invariant 18's non-oracle rule), so
		// this page must too. "We've sent a link if that address is registered" is the only honest
		// phrasing — anything that said "check your inbox" unconditionally would be a lie half the
		// time, and anything that said "no such account" would rebuild the oracle in the UI.
		return { form, sent: true };
	}
};
