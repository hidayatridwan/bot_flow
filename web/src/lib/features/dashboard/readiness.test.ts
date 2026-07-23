import { describe, expect, it } from 'vitest';
import { countDocuments, formatCount, readinessSteps } from './readiness';
import type { Document } from '$lib/types/documents';

const doc = (status: Document['status']): Document => ({
	id: crypto.randomUUID(),
	filename: 'x.md',
	status,
	failureReason: null,
	createdAt: '2026-07-20 00:00:00+00'
});

describe('countDocuments', () => {
	it('counts only what the tenant can act on', () => {
		const counts = countDocuments(
			[doc('ready'), doc('ready'), doc('processing'), doc('uploading'), doc('failed')],
			false
		);
		expect(counts).toEqual({ ready: 2, processing: 2, failed: 1, partial: false });
	});

	it('treats uploading as in-flight, not as failed', () => {
		// From the tenant's side `uploading` and `processing` mean the same thing: not answerable yet,
		// and nothing for me to do. Counting it as failed would send someone to fix a healthy upload.
		expect(countDocuments([doc('uploading')], false).processing).toBe(1);
		expect(countDocuments([doc('uploading')], false).failed).toBe(0);
	});

	it('counts quarantined as needing attention', () => {
		expect(countDocuments([doc('quarantined')], false).failed).toBe(1);
	});

	it('ignores statuses that are neither answerable nor actionable', () => {
		// `expired` is a lapsed upload URL — nothing broke and nothing is indexing.
		const counts = countDocuments([doc('expired'), doc('unknown')], false);
		expect(counts).toEqual({ ready: 0, processing: 0, failed: 0, partial: false });
	});
});

describe('formatCount — never state a total we do not have', () => {
	it('marks a partial page so the number is not read as a total', () => {
		// The API returns no `total` on purpose: a count is the full scan keyset pagination replaced.
		// Rendering "200" when there are 5,000 documents would be a number that quietly means
		// "the first page", which is worse than an obviously approximate one.
		expect(formatCount(200, true)).toBe('200+');
		expect(formatCount(12, false)).toBe('12');
	});
});

describe('readinessSteps — the checklist must say what to do next', () => {
	const none = { ready: 0, processing: 0, failed: 0, partial: false };

	it('blocks the later steps until a document is answerable', () => {
		// Invariant 4: with nothing indexed the bot declines every question, so minting a key or
		// opening the playground is not the next useful thing.
		const steps = readinessSteps(none, 0);
		expect(steps.map((s) => s.state)).toEqual(['pending', 'blocked', 'blocked']);
	});

	it('opens the key step once something is answerable', () => {
		const steps = readinessSteps({ ...none, ready: 3 }, 0);
		expect(steps[0].state).toBe('done');
		expect(steps[1].state).toBe('pending');
		expect(steps[2].state).toBe('pending');
	});

	it('is fully done only with both a ready document and a publishable key', () => {
		const steps = readinessSteps({ ...none, ready: 1 }, 1);
		expect(steps[0].state).toBe('done');
		expect(steps[1].state).toBe('done');
	});

	it('a secret key alone does not satisfy the key step', () => {
		// The caller passes the count of *publishable* keys specifically: an `sk_` cannot drive the
		// widget, and every tenant already has one from registration — so counting all keys would mark
		// this done for everyone, on day one, forever.
		const steps = readinessSteps({ ...none, ready: 1 }, 0);
		expect(steps[1].state).toBe('pending');
	});

	it('says something is indexing rather than implying nothing was uploaded', () => {
		const steps = readinessSteps({ ...none, processing: 2 }, 0);
		expect(steps[0].detail).toMatch(/indexing/i);
		expect(steps[0].detail).not.toMatch(/with nothing indexed/i);
	});

	it('carries the partial marker into its own copy', () => {
		const steps = readinessSteps({ ...none, ready: 200, partial: true }, 1);
		expect(steps[0].detail).toContain('200+');
	});

	it('points every step at a route that exists', () => {
		// The whole point of this phase: a link that goes nowhere costs more trust than a missing
		// feature. These four are real routes.
		const real = new Set(['/documents', '/keys', '/playground']);
		for (const step of readinessSteps(none, 0)) {
			expect(real.has(step.href), `${step.id} -> ${step.href}`).toBe(true);
		}
	});
});
