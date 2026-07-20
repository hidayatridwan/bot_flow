import { setError, superValidate } from 'sveltekit-superforms';
import { zod4 } from 'sveltekit-superforms/adapters';
import type { Actions, PageServerLoad } from './$types';
import { api } from '$lib/server/api';
import * as authApi from '$lib/server/api/auth';
import { requireSession } from '$lib/server/auth/guard';
import { mapChangePasswordError, toFailStatus } from '$lib/features/auth/error-map';
import { changePasswordSchema } from '$lib/features/auth/schema';

export const load: PageServerLoad = async (event) => {
	requireSession(event.locals, event.url);
	return { form: await superValidate(zod4(changePasswordSchema)) };
};

export const actions: Actions = {
	default: async (event) => {
		const { token } = requireSession(event.locals, event.url);
		const form = await superValidate(event, zod4(changePasswordSchema));

		const currentPassword = form.data.currentPassword;
		const newPassword = form.data.newPassword;

		// Three secrets on this form, none of which may survive into the SSR html.
		form.data.currentPassword = '';
		form.data.newPassword = '';
		form.data.confirmPassword = '';

		if (!form.valid) return setError(form, 'Check the fields below.', { status: 400 });

		const res = await authApi.changePassword(api(event, token), {
			current_password: currentPassword,
			new_password: newPassword
		});

		if (!res.ok) {
			const { field, message } = mapChangePasswordError(res.error);
			return field
				? setError(form, field, message, { status: toFailStatus(res.error) })
				: setError(form, message, { status: toFailStatus(res.error) });
		}

		// No redirect and no cookie change: the API keeps *this* session alive and revokes the others,
		// so the user stays exactly where they are. Logging them out of the tab they just used would
		// be punishing the person who did the right thing.
		return { form, changed: true };
	}
};
