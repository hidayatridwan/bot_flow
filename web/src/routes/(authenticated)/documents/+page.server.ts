import type { PageServerLoad } from './$types';
import { api } from '$lib/server/api';
import * as documentsApi from '$lib/server/api/documents';
import { requireSession } from '$lib/server/auth/guard';
import { toStatus } from '$lib/features/documents/status';
import type { Document } from '$lib/types/documents';

export const load: PageServerLoad = async (event) => {
	const { token } = requireSession(event.locals, event.url);

	// Named so the poll can re-run exactly this load, and nothing else.
	event.depends('documents:list');

	const res = await documentsApi.listDocuments(api(event, token));

	if (!res.ok) {
		// Deliberately not `error()`. An empty table would tell the tenant their library is gone and
		// invite them to re-upload everything — the same reasoning as invariant 21 ("an API outage is
		// not a logout"): an API outage is not an empty library. The page renders an alert instead.
		return { documents: [] as Document[], loadError: true };
	}

	// snake_case → domain, and the untrusted `status` string is narrowed here.
	const documents: Document[] = res.data.documents.map((d) => ({
		id: d.id,
		filename: d.filename,
		status: toStatus(d.status),
		createdAt: d.created_at
	}));

	// The token is not in this return value, and must never be (invariant 20).
	return { documents, loadError: false };
};
