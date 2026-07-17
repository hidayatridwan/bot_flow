import type { Source } from './ask';

/**
 * Citations → what the panel renders. The first surface in this product that has ever shown them.
 *
 * That is not a throwaway remark: invariant 5 forbids the model from writing `[n]` markers precisely
 * so citations can be structured data instead of prose, and `widget.js` ignores the `sources` event.
 * So the whole point of that invariant has, until now, had nowhere to land.
 */

export interface SourceDisplay {
	/** Straight from the API. See `label` for why this is not an array position. */
	readonly index: number;
	/** What the chip shows: `[1]`, `[2]`… */
	readonly label: string;
	/** The document's filename, or an honest stand-in. Never a bare UUID. */
	readonly name: string;
	/** `true` when we could not name the document — the row is real, its origin is not knowable. */
	readonly unattributed: boolean;
	readonly score: string;
	readonly text: string;
}

/**
 * A chunk the API cannot attribute.
 *
 * `POST /ingest` writes vectors with **no `document_id` payload**, so its chunks answer questions
 * while belonging to no record — CLAUDE.md calls this the largest single piece of debt in the system.
 * This page is where a tenant would first meet it, and a blank chip would read as a rendering bug
 * rather than the data-lifecycle hole it is. Naming it is the honest option.
 */
export const UNATTRIBUTED = 'Unattributed passage';

/**
 * Join a citation to its document.
 *
 * `filenames` comes from `GET /documents`; a miss is expected rather than exceptional — an `/ingest`
 * chunk has no id at all, and a document deleted from under a live conversation would be another
 * (except there is no delete path yet, which is its own entry in the debt list).
 */
export function toDisplay(source: Source, filenames: Record<string, string>): SourceDisplay {
	const name = source.documentId ? filenames[source.documentId] : undefined;

	return {
		// **Never `i + 1`.** `index` is 1-based and authored by the API, and because the model may not
		// write markers, it is the only thing tying a sentence back to a passage. The two agree today —
		// the API always sends `1..n` in order — so a renumbering refactor would look correct in every
		// test that did not pin this exact field. `sources.test.ts` pins it with a 7.
		index: source.index,
		label: `[${source.index}]`,
		name: name ?? UNATTRIBUTED,
		unattributed: !name,
		score: formatScore(source.score),
		text: source.text
	};
}

/**
 * A relevance score as two decimals.
 *
 * Deliberately not a percentage. Cosine similarity is not a probability, and `54%` invites a tenant
 * to read "54% confident" — which would be a claim the number does not make. `0.54` is opaque enough
 * to prompt the right question and honest enough to survive it.
 */
export function formatScore(score: number): string {
	return score.toFixed(2);
}

export const toDisplays = (sources: Source[], filenames: Record<string, string>): SourceDisplay[] =>
	sources.map((s) => toDisplay(s, filenames));
