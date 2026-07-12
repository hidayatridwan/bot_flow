import { redirect } from '@sveltejs/kit';
import type { PageServerLoad } from './$types';
import { takeApiKeyFlash } from '$lib/server/auth/cookies';

export const load: PageServerLoad = async ({ cookies }) => {
	// Reads and deletes in the same request, so the key is shown exactly once — which is what the API
	// itself promises. A refresh therefore loses it, and that is correct rather than a bug: the key is
	// unrecoverable by design, and POST /auth/keys is the recovery path.
	const apiKey = takeApiKeyFlash(cookies);
	if (!apiKey) redirect(303, '/dashboard');

	return { apiKey };
};
