import { redirect } from '@sveltejs/kit';
import type { LayoutServerLoad } from './$types';
import { safeRedirectTo } from '$lib/utils/redirect';

export const load: LayoutServerLoad = async ({ locals, url }) => {
	if (locals.user) redirect(303, safeRedirectTo(url.searchParams.get('redirectTo')));
	return {};
};
