import type { PageServerLoad } from './$types';
import { api } from '$lib/server/api';
import * as documentsApi from '$lib/server/api/documents';
import { MAX_PAGE_LIMIT } from '$lib/server/api/documents';
import * as keysApi from '$lib/server/api/keys';
import { requireSession } from '$lib/server/auth/guard';
import { toStatus, toFailureReason } from '$lib/features/documents/status';
import { countDocuments } from '$lib/features/dashboard/readiness';
import type { Document } from '$lib/types/documents';

export const load: PageServerLoad = async (event) => {
	const { token } = requireSession(event.locals, event.url);
	const client = api(event, token);

	// Two independent reads, so they go in parallel rather than in sequence — this is the landing
	// page after login and it should not cost two serial round trips.
	const [docsRes, keysRes] = await Promise.all([
		documentsApi.listDocuments(client, { limit: MAX_PAGE_LIMIT }),
		keysApi.listKeys(client)
	]);

	// An outage is not an empty library and not an empty key list — the same reasoning as /documents
	// and /keys. Rendering "0 documents ready" during a blip would tell a tenant their bot is broken
	// and invite them to re-upload everything.
	const loadError = !docsRes.ok || !keysRes.ok;

	const documents: Document[] = docsRes.ok
		? docsRes.data.documents.map((d) => ({
				id: d.id,
				filename: d.filename,
				status: toStatus(d.status),
				failureReason: toFailureReason(d.failure_reason),
				createdAt: d.created_at
			}))
		: [];

	// `next_cursor` non-null means there is another page, so these counts are a floor rather than a
	// total. The API returns no `total` on purpose (a count is the full scan pagination replaced), so
	// this is the most the dashboard can honestly know.
	const counts = countDocuments(documents, docsRes.ok && docsRes.data.next_cursor !== null);

	const publishableKeys = keysRes.ok
		? keysRes.data.keys.filter((k) => k.kind === 'publishable').length
		: 0;

	// The token is not in this return value, and must never be (invariant 20).
	return { counts, publishableKeys, loadError };
};
