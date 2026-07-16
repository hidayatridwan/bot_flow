import type { DocumentStatus } from '$lib/types/documents';

/**
 * Status → what the user is told. This file is where invariant 16 is enforced *in the UI*.
 *
 * The API deliberately does not expose the `documents.error` column: it holds raw parser stderr and
 * our own "worker presumed dead" post-mortem. So everything a user learns about a failure is written
 * here, from the status alone, and none of it may describe our internals.
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

export function toDisplay(status: DocumentStatus | 'unknown'): StatusDisplay {
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
			// Names both causes on purpose. This status conflates a broken file with a dead worker of
			// ours, and we cannot tell them apart without the `error` column. "Your file is corrupt"
			// would blame the user for our outage; "something went wrong on our end" would excuse a
			// genuinely broken PDF. Re-uploading is the right move under both, which is what makes the
			// ambiguity survivable rather than merely vague.
			return {
				label: 'Failed',
				description:
					"We couldn't process this file. This can happen if the file is damaged, or if " +
					'something went wrong on our side. Try uploading it again.',
				variant: 'destructive',
				spinner: false
			};
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
