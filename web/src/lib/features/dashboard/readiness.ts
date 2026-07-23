import type { Document } from '$lib/types/documents';

/**
 * "Will my bot answer a question right now?" — the only thing a dashboard here can usefully say.
 *
 * The page this replaces was one line reading `dashboard tenant`, and it is where onboarding sends
 * every new tenant. The temptation for a replacement is a wall of charts; this deliberately answers
 * one question instead, because it is the question someone has on the day they sign up and the one
 * they cannot answer for themselves without clicking through three pages.
 *
 * **Every number here is derived from data the API actually returns.** Nothing is invented, and the
 * one thing that cannot be known — a true total — is reported as unknown rather than guessed at.
 */

export type StepState = 'done' | 'pending' | 'blocked';

export interface ReadinessStep {
	readonly id: 'documents' | 'key' | 'try';
	readonly title: string;
	readonly detail: string;
	readonly state: StepState;
	readonly href: string;
	readonly cta: string;
}

export interface DocumentCounts {
	readonly ready: number;
	readonly processing: number;
	readonly failed: number;
	/**
	 * True when the counts cover only the first page — there are more documents than were fetched.
	 *
	 * The API returns **no `total`**, deliberately: a count is the full scan keyset pagination exists
	 * to avoid. So a dashboard cannot know how many documents a tenant has, and the honest move is to
	 * say "200+" rather than to render a number that quietly means "the first 200".
	 */
	readonly partial: boolean;
}

/** Tally a page of documents by the only three statuses this page reasons about. */
export function countDocuments(documents: Document[], partial: boolean): DocumentCounts {
	let ready = 0;
	let processing = 0;
	let failed = 0;
	for (const d of documents) {
		if (d.status === 'ready') ready++;
		// `uploading` counts as in-flight too: from the tenant's side both mean "not answerable yet,
		// and nothing for me to do".
		else if (d.status === 'processing' || d.status === 'uploading') processing++;
		else if (d.status === 'failed' || d.status === 'quarantined') failed++;
	}
	return { ready, processing, failed, partial };
}

/** Render a count that may be an undercount. */
export function formatCount(n: number, partial: boolean): string {
	return partial ? `${n}+` : String(n);
}

/**
 * The go-live checklist, in the order the steps actually gate each other.
 *
 * `blocked` rather than `pending` for a step whose precondition is missing: minting a key before any
 * document is indexed is not wrong, but it is not the next useful thing either, and a checklist that
 * cannot say "do this one next" is a list of links.
 */
export function readinessSteps(docs: DocumentCounts, publishableKeys: number): ReadinessStep[] {
	const hasReady = docs.ready > 0;
	const hasKey = publishableKeys > 0;

	return [
		{
			id: 'documents',
			title: 'Index a document',
			detail: hasReady
				? `${formatCount(docs.ready, docs.partial)} ready to answer from.`
				: docs.processing > 0
					? `${docs.processing} still indexing. This usually takes a few seconds.`
					: 'Your bot answers only from documents you upload — with nothing indexed, it declines every question.',
			state: hasReady ? 'done' : 'pending',
			href: '/documents',
			cta: hasReady ? 'Manage documents' : 'Upload a document'
		},
		{
			id: 'key',
			title: 'Create a publishable key',
			detail: hasKey
				? 'Your widget can authenticate. Copy the embed snippet from the keys page.'
				: 'The embeddable widget needs a `pk_` key locked to the domains you will embed on.',
			state: hasKey ? 'done' : hasReady ? 'pending' : 'blocked',
			href: '/keys',
			cta: hasKey ? 'View keys' : 'Create a key'
		},
		{
			id: 'try',
			title: 'Ask it something',
			detail: hasReady
				? 'Check the answers against your own documents before you embed it anywhere.'
				: 'Available once a document has finished indexing.',
			state: hasReady ? 'pending' : 'blocked',
			href: '/playground',
			cta: 'Open the playground'
		}
	];
}
