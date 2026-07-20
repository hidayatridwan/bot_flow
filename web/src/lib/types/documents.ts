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
	/** Terminal failure. `failureReason` says whose fault, and therefore what to do about it. */
	| 'failed'
	/** The upload never arrived and the reaper settled the row. */
	| 'expired'
	/** Broke a rule no retry can fix. Today that means exactly one thing: too large. */
	| 'quarantined';

/**
 * Why a document failed, in terms the tenant can act on.
 *
 * The mirror of `FailureReason` in `crates/worker/src/failure.rs`, and the third of the three
 * places this enum lives — the DB `CHECK` in migration 0015 is the one that fails closed. The cut
 * is by **what to do**, not by what broke: everything on our side of the line is `system_error`,
 * because a tenant cannot act on the difference between Qdrant and MinIO being down, and naming it
 * would leak our topology (invariant 16).
 */
export type FailureReason =
	/** Right file type, unreadable contents — encrypted, truncated, malformed. Re-upload. */
	| 'unreadable_file'
	/** We do not support this format. Convert it. */
	| 'unsupported_type'
	/** Over the upload cap; the bytes were discarded. Upload something smaller. */
	| 'too_large'
	/** Our fault: sidecar crash, embedding failure, dead worker. Wait — do not re-upload. */
	| 'system_error';

export interface Document {
	readonly id: string;
	readonly filename: string;
	/** 'unknown' when the API sends a status this build does not recognise. */
	readonly status: DocumentStatus | 'unknown';
	/**
	 * `null` for anything that has not failed, and for rows that failed *before* the worker
	 * classified failures — nothing ever recorded a cause for those, so the UI says only what the
	 * old copy said rather than inventing a verdict.
	 */
	readonly failureReason: FailureReason | null;
	readonly createdAt: string;
}
