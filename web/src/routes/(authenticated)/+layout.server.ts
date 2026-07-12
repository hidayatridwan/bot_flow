import type { LayoutServerLoad } from './$types';
import { requireUser } from '$lib/server/auth/guard';

export const load: LayoutServerLoad = async ({ locals, url }) => {
	const user = requireUser(locals, url);
	// `user` and nothing else. `locals.session.token` never crosses this line.
	return { user };
};
