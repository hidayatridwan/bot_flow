import type { DocumentStatus, FailureReason } from '$lib/types/documents';

/**
 * Status → what the user is told. This file is where invariant 16 is enforced *in the UI*.
 *
 * The API still does not expose the `documents.error` column — it holds raw parser stderr and our
 * own "worker presumed dead" post-mortem, and none of that may describe our internals to a tenant.
 * What it *does* expose is `failure_reason`, a closed enum whose entire purpose is to be shown. So
 * the copy below is written from the status **and** that reason, and from nothing else.
 *
 * The distinction the reason buys is the only one a tenant can act on: **re-upload, or wait.**
 * Before it existed, `failed` had to name both causes at once, and someone whose worker had died
 * was told to re-upload a file that was never broken. Getting this backwards is worse than the
 * ambiguity it replaces, which is why `system_error` — the catch-all — is the branch that must
 * never tell anyone to re-upload.
 */

export type StatusVariant = 'secondary' | 'destructive' | 'outline';

export interface StatusDisplay {
	readonly label: string;
	readonly description: string;
	readonly variant: StatusVariant;
	/** Transient states get a spinner: something is still happening. */
	readonly spinner: boolean;
}

const ALL: readonly DocumentStatus[] = [
	'uploading',
	'processing',
	'ready',
	'failed',
	'expired',
	'quarantined'
];

/** Exported for tests: the set the display map must cover exhaustively. */
export const ALL_STATUSES = ALL;

const ALL_REASONS: readonly FailureReason[] = [
	'unreadable_file',
	'unsupported_type',
	'too_large',
	'system_error'
];

/** Exported for tests: the set `toDisplay` must handle for a `failed` document. */
export const ALL_FAILURE_REASONS = ALL_REASONS;

/**
 * Narrow an untrusted wire reason, same contract as `toStatus` — but note the fallback differs on
 * purpose. An unrecognised *status* becomes `'unknown'` and gets its own branch; an unrecognised
 * *reason* becomes `null`, which renders as the cause-agnostic copy.
 *
 * That is the safe direction. A reason this build predates is one we cannot explain, and guessing
 * would either blame a tenant for our outage or excuse a broken file — the two failure modes this
 * whole column exists to prevent. Saying less is the correct degradation.
 */
export function toFailureReason(raw: string | null | undefined): FailureReason | null {
	return raw != null && (ALL_REASONS as readonly string[]).includes(raw)
		? (raw as FailureReason)
		: null;
}

/**
 * Narrow an untrusted wire string. Anything unrecognised becomes 'unknown' rather than being cast —
 * the API may ship a status this build predates (the DB CHECK already carries a writerless
 * `uploaded`), and a bad cast would mis-render it as whatever the switch fell through to.
 */
export function toStatus(raw: string): DocumentStatus | 'unknown' {
	return (ALL as readonly string[]).includes(raw) ? (raw as DocumentStatus) : 'unknown';
}

/**
 * Is the system still working on this? Only these two move on their own, so only these two justify
 * polling.
 *
 * `failed` is *not* transient here, though the worker can in fact re-claim a failed row if AMQP
 * redelivers. That leaves the UI briefly stale in a rare case, which is the right trade: treating
 * `failed` as transient would poll every abandoned row for the 20 minutes it takes the reaper to
 * settle it.
 */
export function isTransient(status: DocumentStatus | 'unknown'): boolean {
	return status === 'uploading' || status === 'processing';
}

/**
 * The `failed` copy, which is the whole reason `failure_reason` exists.
 *
 * Two of these branches say "try again" and two say "we're on it", and that split is the product
 * decision — not the wording. `null` keeps the original both-causes copy, because a row that failed
 * before classification genuinely has no known cause and inventing one would be worse than vague.
 */
function failedDisplay(reason: FailureReason | null): StatusDisplay {
	const base = { label: 'Failed', variant: 'destructive', spinner: false } as const;
	switch (reason) {
		case 'unreadable_file':
			return {
				...base,
				description:
					"We couldn't read this file — it may be damaged, password-protected, or a scan with " +
					'no text in it. Try uploading it again, or export a fresh copy.'
			};
		case 'unsupported_type':
			return {
				...base,
				description: "We can't read this file type. Upload a PDF, TXT or MD file instead."
			};
		case 'too_large':
			// Reachable in principle, though the size cap normally lands on `quarantined` before a
			// claim. Kept because the enum is closed and a silent fall-through would be a lie.
			return {
				...base,
				description: 'This file is over the 25 MB limit. Upload a smaller file.'
			};
		case 'system_error':
			// The one branch that must NOT suggest re-uploading. The tenant's file is very likely
			// fine; something on our side failed, and telling them to re-upload sends them round a
			// loop that cannot succeed while blaming them for our outage.
			return {
				...base,
				description:
					"Something went wrong on our side while processing this file — it's not a problem " +
					"with your document. We're looking into it; this will retry automatically."
			};
		case null:
			// Pre-classification rows. Says exactly as much as is actually known.
			return {
				...base,
				description:
					"We couldn't process this file. This can happen if the file is damaged, or if " +
					'something went wrong on our side. Try uploading it again.'
			};
	}
}

export function toDisplay(
	status: DocumentStatus | 'unknown',
	reason: FailureReason | null = null
): StatusDisplay {
	switch (status) {
		case 'uploading':
			return {
				label: 'Uploading',
				description: 'Waiting for the file to arrive.',
				variant: 'secondary',
				spinner: true
			};
		case 'processing':
			return {
				label: 'Processing',
				description: 'Reading and indexing this file. This usually takes a few seconds.',
				variant: 'secondary',
				spinner: true
			};
		case 'ready':
			return {
				label: 'Ready',
				description: 'Ready to answer questions.',
				variant: 'secondary',
				spinner: false
			};
		case 'failed':
			return failedDisplay(reason);
		case 'expired':
			// Not destructive: nothing broke. The usual cause is a closed tab.
			return {
				label: 'Upload expired',
				description: "The upload didn't finish in time. Upload the file again.",
				variant: 'outline',
				spinner: false
			};
		case 'quarantined':
			// The one specific failure we can honestly name: the oversize check is this status's only
			// writer today. If a second writer ever appears, this copy becomes a lie — the test pins it.
			return {
				label: 'Too large',
				description:
					"This file is over the 25 MB limit, so we couldn't keep it. Upload a smaller file.",
				variant: 'destructive',
				spinner: false
			};
		case 'unknown':
			return {
				label: 'Unknown',
				description: "We're not sure about this file's status. Try reloading.",
				variant: 'outline',
				spinner: false
			};
	}
}
