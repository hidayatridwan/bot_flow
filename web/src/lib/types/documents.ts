/**
 * The domain type. The wire shape lives in `$lib/server/api/documents.ts`; this is what the rest of
 * the app speaks — camelCase, and a `status` that has already been narrowed.
 */

/**
 * Every status a document can actually hold.
 *
 * The DB CHECK also permits `uploaded`, but no code path in the system assigns it. It is a vestige,
 * and a branch for it here would be dead code that looks load-bearing. If it ever gains a writer,
 * `toStatus` returns 'unknown' for it and the UI degrades safely rather than mis-rendering.
 */
export type DocumentStatus =
	/** The row exists and a URL was minted, but no bytes have arrived yet. */
	| 'uploading'
	/** A worker holds the lease and is parsing/embedding. */
	| 'processing'
	/** Terminal success: chunks are indexed and answerable. */
	| 'ready'
	/** A parse failure *or* a worker that died mid-lease. The two are indistinguishable from here. */
	| 'failed'
	/** The upload never arrived and the reaper settled the row. */
	| 'expired'
	/** Broke a rule no retry can fix. Today that means exactly one thing: too large. */
	| 'quarantined';

export interface Document {
	readonly id: string;
	readonly filename: string;
	/** 'unknown' when the API sends a status this build does not recognise. */
	readonly status: DocumentStatus | 'unknown';
	readonly createdAt: string;
}
