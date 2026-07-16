import { error, json } from '@sveltejs/kit';
import type { RequestHandler } from './$types';
import { api } from '$lib/server/api';
import * as documentsApi from '$lib/server/api/documents';

/**
 * Mint (or re-mint) a presigned upload URL for the logged-in tenant.
 *
 * **Why the browser is handed a URL that writes to storage.** This looks like a leak on a skim; it
 * is not. The session token stays in this process — it is read from `locals` and sent to the API as
 * a Bearer header, server to server. What goes back to the browser is a presigned URL, which
 * authorises exactly one object key, one method, for one TTL. That is a capability, not a
 * credential, and handing it out is the whole point of `POST /documents/upload-url`: the bytes go
 * straight to storage without transiting Node (see invariant 11 and the deprecated multipart route).
 *
 * This is the app's first non-form-action surface, so CSRF is worth naming. `bf_session` is
 * `SameSite=Lax`, so a cross-site `fetch` POST does not carry it — that alone closes this. Note that
 * SvelteKit's `csrf.checkOrigin` does *not* apply here: it only covers form-encodable content types,
 * so it never sees a JSON POST. The explicit checks below are therefore the belt to Lax's braces.
 */

const isUuid = (s: unknown): s is string =>
	typeof s === 'string' &&
	/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(s);

export const POST: RequestHandler = async (event) => {
	const { request, url, locals } = event;

	if (request.headers.get('origin') !== url.origin) {
		error(403, 'forbidden');
	}
	if (!request.headers.get('content-type')?.includes('application/json')) {
		error(415, 'expected application/json');
	}

	// NOT `requireUser`/`requireSession`: those throw a 303 to /login, and `fetch` follows redirects —
	// the caller would get the login page's HTML with a 200 and choke parsing it as JSON. A JSON
	// endpoint answers with a status the client can branch on.
	if (!locals.session) {
		error(401, 'not signed in');
	}

	let body: unknown;
	try {
		body = await request.json();
	} catch {
		error(400, 'invalid json');
	}

	const client = api(event, locals.session.token);
	const { filename, documentId } = (body ?? {}) as { filename?: unknown; documentId?: unknown };

	// One endpoint, two shapes. Mirroring the API 1:1 would mean two routes that shadow each other,
	// two guards, and two places to forget the origin check — for an identical response shape.
	const res = isUuid(documentId)
		? // Re-mint: only ever for the file the row was created for. `upload.ts` guarantees that by
			// holding the same `File`; the id is validated above so it cannot inject into the path.
			await documentsApi.refreshUploadUrl(client, documentId)
		: typeof filename === 'string' && filename.length > 0
			? await documentsApi.createUploadUrl(client, { filename })
			: error(400, 'expected a filename or a documentId');

	if (!res.ok) {
		// Hand the typed error down so the client can map it to copy. `kind` is what lets the client
		// tell "you did something wrong" from "we are broken" without reading `message`.
		return json({ error: res.error }, { status: res.error.status || 503 });
	}

	return json({
		documentId: res.data.document_id,
		uploadUrl: res.data.upload_url,
		expiresAt: res.data.expires_at
	});
};
