import { fail } from '@sveltejs/kit';
import { setError, superValidate } from 'sveltekit-superforms';
import { zod4 } from 'sveltekit-superforms/adapters';
import type { Actions, PageServerLoad } from './$types';
import { api } from '$lib/server/api';
import * as keysApi from '$lib/server/api/keys';
import { requireSession } from '$lib/server/auth/guard';
import { widgetApiBaseUrl } from '$lib/server/env';
import { mapKeyError } from '$lib/features/keys/error-map';
import { mintSchema, parseOrigins } from '$lib/features/keys/schema';

export const load: PageServerLoad = async (event) => {
	const { token } = requireSession(event.locals, event.url);
	event.depends('keys:list');

	const res = await keysApi.listKeys(api(event, token));

	return {
		// The snippet needs the URL a tenant's *visitors* reach, not this server's API_BASE_URL.
		widgetApiBase: widgetApiBaseUrl(),
		form: await superValidate(zod4(mintSchema)),
		keys: res.ok ? res.data.keys : [],
		// An API outage is not an empty key list — the same reasoning as /documents. Showing "no keys"
		// here would invite the tenant to mint duplicates of keys they already have.
		loadError: !res.ok
	};
	// No token in this return value (invariant 20).
};

export const actions: Actions = {
	mint: async (event) => {
		const { token } = requireSession(event.locals, event.url);
		const form = await superValidate(event, zod4(mintSchema));
		if (!form.valid) return fail(400, { form });

		const res = await keysApi.createKey(api(event, token), {
			kind: form.data.kind,
			label: form.data.label || undefined,
			allowed_origins: parseOrigins(form.data.origins)
		});

		if (!res.ok) return setError(form, 'origins', mapKeyError(res.error), { status: 422 });

		// The raw key rides back in the action result and is rendered once. Reload and it is gone —
		// which is invariant 22 holding, not a bug to paper over. No flash cookie is needed because,
		// unlike register, this does not redirect.
		return {
			form,
			minted: {
				apiKey: res.data.api_key,
				kind: res.data.kind,
				allowedOrigins: res.data.allowed_origins
			}
		};
	},

	updateOrigins: async (event) => {
		const { token } = requireSession(event.locals, event.url);
		const data = await event.request.formData();
		const keyHash = String(data.get('keyHash') ?? '');
		const origins = parseOrigins(String(data.get('origins') ?? ''));

		// The API re-validates and re-normalises; it is the authority. This only avoids a round trip.
		const res = await keysApi.updateKeyOrigins(api(event, token), keyHash, {
			allowed_origins: origins
		});

		if (!res.ok) return fail(res.error.status || 503, { updateError: mapKeyError(res.error) });
		return { updated: true };
	},

	revoke: async (event) => {
		const { token } = requireSession(event.locals, event.url);
		const data = await event.request.formData();
		const keyHash = String(data.get('keyHash') ?? '');

		const res = await keysApi.revokeKey(api(event, token), keyHash);
		if (!res.ok) return fail(res.error.status || 503, { revokeError: mapKeyError(res.error) });
		return { revoked: true };
	}
};
