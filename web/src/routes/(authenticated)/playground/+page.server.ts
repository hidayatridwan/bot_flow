import type { PageServerLoad } from './$types';
import { api } from '$lib/server/api';
import * as documentsApi from '$lib/server/api/documents';
import { requireSession } from '$lib/server/auth/guard';

/**
 * The playground's data, which is only ever *about* the answer — the asking itself goes through
 * `playground/ask`, because a stream is not a `load` return value.
 *
 * Note there is no `form` and no action. This page is the one place in the dashboard that cannot be a
 * form action, and `ask/+server.ts` records why.
 */
export const load: PageServerLoad = async (event) => {
	const { token } = requireSession(event.locals, event.url);

	const res = await documentsApi.listDocuments(api(event, token));

	// `document_id` → filename, so a citation can name its document instead of showing a UUID. This is
	// the entire reason the page reads the document list at all.
	const filenames: Record<string, string> = {};
	let readyCount = 0;
	if (res.ok) {
		for (const doc of res.data.documents) {
			filenames[doc.id] = doc.filename;
			if (doc.status === 'ready') readyCount += 1;
		}
	}

	return {
		filenames,
		readyCount,
		// An API outage is not "no documents" — the same reasoning as /documents and /keys. Silently
		// degrading every citation to "Unattributed passage" would blame the data for our outage.
		loadError: !res.ok
	};
	// No token in this return value (invariant 20).
};
