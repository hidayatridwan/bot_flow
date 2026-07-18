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
	created_at: string;
}

export interface ListDocumentsResponse {
	documents: DocumentDto[];
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

/** Returns the tenant's entire table — no pagination server-side. Scoped by Postgres RLS. */
export const listDocuments = (client: ApiClient) => client.get<ListDocumentsResponse>('/documents');

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
