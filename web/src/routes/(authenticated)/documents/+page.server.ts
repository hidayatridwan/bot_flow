import { fail } from '@sveltejs/kit';
import type { Actions, PageServerLoad } from './$types';
import { api } from '$lib/server/api';
import * as documentsApi from '$lib/server/api/documents';
import { requireSession } from '$lib/server/auth/guard';
import { toFailureReason, toStatus } from '$lib/features/documents/status';
import { genericMessage } from '$lib/utils/api-copy';
import type { Document } from '$lib/types/documents';

export const load: PageServerLoad = async (event) => {
	const { token } = requireSession(event.locals, event.url);

	// Named so the poll can re-run exactly this load, and nothing else.
	event.depends('documents:list');

	// The cursor lives in the URL rather than in component state, and that is load-bearing twice
	// over. `invalidate` re-runs this load with the *same* URL, so polling refreshes the page the
	// tenant is actually looking at instead of yanking them back to the newest rows. And because
	// each page is a real URL, the browser's own Back button walks the pages — which is what keeps
	// this list readable with no JavaScript (invariant 24).
	const before = event.url.searchParams.get('before');

	const res = await documentsApi.listDocuments(api(event, token), { before });

	if (!res.ok) {
		// Deliberately not `error()`. An empty table would tell the tenant their library is gone and
		// invite them to re-upload everything — the same reasoning as invariant 21 ("an API outage is
		// not a logout"): an API outage is not an empty library. The page renders an alert instead.
		return { documents: [] as Document[], loadError: true, nextCursor: null, isFirstPage: true };
	}

	// snake_case → domain, and the untrusted `status` string is narrowed here.
	const documents: Document[] = res.data.documents.map((d) => ({
		id: d.id,
		filename: d.filename,
		status: toStatus(d.status),
		failureReason: toFailureReason(d.failure_reason),
		createdAt: d.created_at
	}));

	// The token is not in this return value, and must never be (invariant 20).
	return {
		documents,
		loadError: false,
		nextCursor: res.data.next_cursor ?? null,
		isFirstPage: before === null
	};
};

export const actions: Actions = {
	delete: async (event) => {
		const { token } = requireSession(event.locals, event.url);
		const id = String((await event.request.formData()).get('id') ?? '');

		const res = await documentsApi.deleteDocument(api(event, token), id);

		// A 404 means it is already gone — from the tenant's side, deleted. Report success rather than
		// an error for a document that is not there (deletion is idempotent). A session only ever
		// addresses its own tenant's ids, so this is not the non-oracle concern the API side guards.
		if (!res.ok && res.error.status !== 404) {
			return fail(res.error.status || 503, { deleteError: genericMessage(res.error) });
		}
		return { deleted: true };
	}
};
