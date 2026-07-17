import { describe, expect, it } from 'vitest';
import type { Source } from './ask';
import { UNATTRIBUTED, formatScore, toDisplay, toDisplays } from './sources';

const source = (over: Partial<Source> = {}): Source => ({
	index: 1,
	score: 0.54,
	documentId: 'doc-1',
	text: 'Refunds are accepted within 30 days.',
	...over
});

const FILENAMES = { 'doc-1': 'handbook.pdf', 'doc-2': 'faq.md' };

describe('toDisplay', () => {
	it('names the document a citation came from', () => {
		// A bare UUID is not a citation a human can act on. This join is why the load calls
		// listDocuments at all.
		const out = toDisplay(source(), FILENAMES);
		expect(out.name).toBe('handbook.pdf');
		expect(out.unattributed).toBe(false);
		expect(out.label).toBe('[1]');
	});

	it('renders index from the field, never from the array position', () => {
		// The assertion this file exists for. The API always sends 1..n in order, so a `#{i+1}`
		// refactor would pass every other test here and be a silent lie the first time the API filters
		// a source. Invariant 5 makes `index` the only route from prose back to a passage.
		const out = toDisplay(source({ index: 7 }), FILENAMES);
		expect(out.index).toBe(7);
		expect(out.label).toBe('[7]');
	});

	it('keeps each source on its own index when mapping a list', () => {
		const out = toDisplays(
			[source({ index: 3, documentId: 'doc-2' }), source({ index: 9, documentId: 'doc-1' })],
			FILENAMES
		);
		expect(out.map((s) => s.label)).toEqual(['[3]', '[9]']);
		expect(out.map((s) => s.name)).toEqual(['faq.md', 'handbook.pdf']);
	});

	describe('a chunk we cannot attribute', () => {
		it('names the gap rather than rendering a blank chip', () => {
			// POST /ingest writes vectors with no document_id at all. The chunk is real and it answered
			// the question; what is missing is any record it belongs to. A blank chip would read as a
			// rendering bug instead of the data-lifecycle hole it actually is.
			const out = toDisplay(source({ documentId: '' }), FILENAMES);
			expect(out.name).toBe(UNATTRIBUTED);
			expect(out.unattributed).toBe(true);
		});

		it('does the same for an id no document claims', () => {
			const out = toDisplay(source({ documentId: 'doc-gone' }), FILENAMES);
			expect(out.name).toBe(UNATTRIBUTED);
			expect(out.unattributed).toBe(true);
		});

		it('never renders undefined, whatever the map holds', () => {
			expect(toDisplay(source({ documentId: 'doc-1' }), {}).name).toBe(UNATTRIBUTED);
			expect(toDisplay(source({ documentId: '' }), {}).name).toBe(UNATTRIBUTED);
		});

		it('still keeps the citation usable', () => {
			// Unattributed is about provenance, not validity: the passage still grounded the answer and
			// the index still maps to it.
			const out = toDisplay(source({ index: 2, documentId: '' }), FILENAMES);
			expect(out.label).toBe('[2]');
			expect(out.text).toBe('Refunds are accepted within 30 days.');
		});
	});
});

describe('formatScore', () => {
	it('shows two decimals, not a percentage', () => {
		// Cosine similarity is not a probability. "54%" invites "54% confident", which is a claim the
		// number does not make.
		expect(formatScore(0.5906534194946289)).toBe('0.59');
		expect(formatScore(0.54)).toBe('0.54');
		expect(formatScore(1)).toBe('1.00');
		expect(formatScore(0)).toBe('0.00');
	});
});
