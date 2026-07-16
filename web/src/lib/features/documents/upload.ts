import type { ApiError } from '$lib/types/api';
import { mapPutError, mapUploadError } from './error-map';
import { validateFile } from './schema';

/**
 * The client-side upload orchestration: ask our own origin for a presigned URL, then PUT the bytes
 * straight to storage.
 *
 * **The session token is not here, and must never be.** The mint request goes to our own origin and
 * the `bf_session` cookie rides along automatically — httpOnly, so this code cannot read it even if
 * it wanted to. The PUT then goes cross-origin to a host we do not control, carrying a URL that
 * authorises exactly one object key, one method, for 15 minutes. That URL is a capability, not a
 * credential: handing it to the browser is the entire point of a presigned upload, and it is not a
 * breach of invariant 20. Attaching an `Authorization` header to the PUT *would* be.
 *
 * `fetch` is injected rather than imported for the same reason `createApiClient` does it: so the
 * boundary above can be asserted in a unit test.
 */

export type UploadOutcome =
	| { readonly ok: true; readonly documentId: string }
	| { readonly ok: false; readonly message: string };

interface MintResponse {
	documentId: string;
	uploadUrl: string;
	expiresAt: string;
}

/** Our own BFF endpoint. Relative on purpose — it must never be an absolute API URL. */
const MINT_PATH = '/documents/upload-url';

export interface UploadDeps {
	fetch: typeof globalThis.fetch;
}

async function mint(body: unknown, deps: UploadDeps): Promise<MintResponse | { error: string }> {
	let res: Response;
	try {
		res = await deps.fetch(MINT_PATH, {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify(body)
		});
	} catch {
		return { error: 'Upload failed. Check your connection and try again.' };
	}

	if (res.status === 401) {
		// The session died mid-session. A page reload lands on the guard, which redirects properly.
		return { error: 'Your session has expired. Please log in again.' };
	}

	let payload: unknown;
	try {
		payload = await res.json();
	} catch {
		return { error: 'Something went wrong. Please try again.' };
	}

	if (!res.ok) {
		const err = payload as { error?: ApiError };
		return {
			error: err?.error ? mapUploadError(err.error) : 'Something went wrong. Please try again.'
		};
	}

	return payload as MintResponse;
}

/**
 * PUT the bytes. No credentials, no auth header — the signature is in the query string.
 *
 * `credentials: 'omit'` is explicit rather than relying on the default: the default for a
 * cross-origin fetch is already 'same-origin' (i.e. no cookies sent), but stating it means a future
 * edit has to actively remove a word to break it.
 */
async function put(url: string, file: File, deps: UploadDeps): Promise<number> {
	try {
		const res = await deps.fetch(url, {
			method: 'PUT',
			body: file,
			credentials: 'omit'
		});
		return res.status;
	} catch {
		return 0;
	}
}

export async function uploadFile(file: File, deps: UploadDeps): Promise<UploadOutcome> {
	const invalid = validateFile(file);
	if (invalid) return { ok: false, message: invalid };

	const minted = await mint({ filename: file.name }, deps);
	if ('error' in minted) return { ok: false, message: minted.error };

	const status = await put(minted.uploadUrl, file, deps);
	if (status >= 200 && status < 300) return { ok: true, documentId: minted.documentId };

	// A 403 means the signature lapsed — the upload outlived the URL's 15 minutes. Re-mint and try
	// once more.
	//
	// This is the ONLY safe use of the re-mint endpoint. It re-signs the row's existing object key,
	// whose extension came from the original filename and is never revalidated. Here the file is
	// identical *by construction* — same `File`, same name — so the extension provably cannot have
	// changed. Re-minting for a file the user picked again would write (say) markdown bytes to
	// `original.pdf`, and the sidecar dispatches on that suffix.
	if (status === 403) {
		const again = await mint({ documentId: minted.documentId }, deps);
		if ('error' in again) return { ok: false, message: again.error };

		const retry = await put(again.uploadUrl, file, deps);
		if (retry >= 200 && retry < 300) return { ok: true, documentId: minted.documentId };

		// A second 403 is not an expiry — a fresh URL cannot already be stale. Something is wrong with
		// the signature itself, and looping would just hammer storage.
		return { ok: false, message: mapPutError(retry) };
	}

	return { ok: false, message: mapPutError(status) };
}
