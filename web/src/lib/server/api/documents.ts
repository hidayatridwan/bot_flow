import type { ApiClient } from './client';

/**
 * The wire shape. snake_case stops here, exactly as in `auth.ts`: everything above this file speaks
 * the `Document` domain type. That boundary is why a rename in the Rust API touches one file.
 */

export interface DocumentDto {
	id: string;
	filename: string;
	/**
	 * Deliberately `string`, not `DocumentStatus`.
	 *
	 * The wire is untrusted. Typing it as the union would make TypeScript believe a claim the API has
	 * not made — and `switch` exhaustiveness would then silently mis-render anything new. The DB CHECK
	 * already holds a `uploaded` value with no writer; the day it gains one, this stays honest.
	 * Narrowing happens in `features/documents/status.ts`, where unknown falls to a safe branch.
	 */
	status: string;
	/**
	 * Also deliberately `string | null`, for the same reason as `status` — and one more: this field
	 * is null both for "did not fail" and for "failed before classification existed", and the second
	 * of those is a fact about old rows the type must not paper over. Narrowed by `toFailureReason`.
	 */
	failure_reason: string | null;
	created_at: string;
}

export interface ListDocumentsResponse {
	documents: DocumentDto[];
	/**
	 * The cursor for the next (older) page, or `null` on the last page.
	 *
	 * Opaque by contract — echo it back untouched. It encodes `(created_at, id)`, and the `id` half
	 * is not decoration: the API's `created_at` is not unique, so a cursor without it would land on
	 * a page boundary that cannot be resolved.
	 */
	next_cursor: string | null;
	/** The page size actually applied, which is the default when the caller named none. */
	limit: number;
}

export interface UploadUrlBody {
	filename: string;
}

export interface UploadUrlResponse {
	document_id: string;
	upload_url: string;
	method: 'PUT';
	expires_at: string;
}

/**
 * One page of the tenant's documents, newest first. Scoped by Postgres RLS.
 *
 * Keyset, not offset: this list is *polled*, and with an offset a document created between two
 * polls shifts every following row by one — so the reader silently sees a row twice or misses one.
 * Pass the previous response's `next_cursor` as `before` to walk backwards in time.
 *
 * Omitting both parameters is a valid call and returns a bounded first page; the API defaults the
 * size rather than returning everything.
 */
export const listDocuments = (client: ApiClient, page: { before?: string | null } = {}) => {
	// URLSearchParams because the cursor carries `+` (the UTC offset) and `:` — and a raw `+` in a
	// query string decodes to a space, which would corrupt the timestamp rather than merely look
	// untidy. Hand-concatenating this is the bug that does not show up until a page boundary.
	const qs = new URLSearchParams();
	if (page.before) qs.set('before', page.before);
	const suffix = qs.size > 0 ? `?${qs}` : '';
	return client.get<ListDocumentsResponse>(`/documents${suffix}`);
};

export const createUploadUrl = (client: ApiClient, body: UploadUrlBody) =>
	client.post<UploadUrlResponse>('/documents/upload-url', body);

/**
 * Re-mint a URL for a row whose upload never landed.
 *
 * Only safe for *the same file* the row was created for. The API re-signs the row's existing
 * `object_key`, whose extension came from the original filename and is never revalidated — so
 * re-minting for a different file type writes (say) markdown bytes to `original.pdf`, and the
 * sidecar, which dispatches on the suffix, then fails a perfectly good file. Callers must hold the
 * original `File`. See `upload.ts`.
 */
export const refreshUploadUrl = (client: ApiClient, documentId: string) =>
	client.post<UploadUrlResponse>(`/documents/${documentId}/upload-url`);

/**
 * Erase a document across all three stores (phase 8).
 *
 * `204` when done inline, `202` when a worker was mid-index and the reaper sweep will finish it —
 * both are `ok`, and both mean the same thing to the tenant: it is gone from their list now. A `404`
 * means it was already gone (idempotent); the caller treats that as success rather than an error.
 */
export const deleteDocument = (client: ApiClient, documentId: string) =>
	client.del<void>(`/documents/${documentId}`);
