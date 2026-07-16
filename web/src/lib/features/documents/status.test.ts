import { describe, expect, it } from 'vitest';
import { ALL_STATUSES, isTransient, toDisplay, toStatus } from './status';

describe('toStatus — the wire is untrusted', () => {
	it('narrows every real status', () => {
		for (const s of ALL_STATUSES) expect(toStatus(s)).toBe(s);
	});

	it('falls back to unknown rather than casting', () => {
		// `uploaded` is in the DB CHECK with no writer. If it ever gains one, the UI must degrade to a
		// safe branch rather than mis-render it as whatever the switch fell through to.
		expect(toStatus('uploaded')).toBe('unknown');
		expect(toStatus('')).toBe('unknown');
		expect(toStatus('READY')).toBe('unknown'); // case matters; the API is lowercase
		expect(toStatus('nonsense')).toBe('unknown');
	});
});

describe('isTransient — what justifies polling', () => {
	it('is exactly uploading and processing', () => {
		expect(isTransient('uploading')).toBe(true);
		expect(isTransient('processing')).toBe(true);

		// Everything else is settled. `failed` in particular: the worker CAN re-claim a failed row on
		// an AMQP redelivery, but treating it as transient would poll every abandoned row for the 20
		// minutes it takes the reaper to settle it.
		expect(isTransient('ready')).toBe(false);
		expect(isTransient('failed')).toBe(false);
		expect(isTransient('expired')).toBe(false);
		expect(isTransient('quarantined')).toBe(false);
		expect(isTransient('unknown')).toBe(false);
	});
});

describe('toDisplay', () => {
	it('covers every status plus unknown', () => {
		for (const s of [...ALL_STATUSES, 'unknown' as const]) {
			const d = toDisplay(s);
			expect(d.label, `${s} needs a label`).toBeTruthy();
			expect(d.description, `${s} needs a description`).toBeTruthy();
		}
	});

	it('spins only while something is actually happening', () => {
		for (const s of [...ALL_STATUSES, 'unknown' as const]) {
			expect(toDisplay(s).spinner, `${s}`).toBe(isTransient(s));
		}
	});

	/**
	 * Invariant 16, enforced where it is displayed.
	 *
	 * The API withholds the `documents.error` column precisely because it holds parser stderr and our
	 * own "worker presumed dead" post-mortem. All of this copy is written from the status alone — so
	 * if any of these words appear, someone has started describing our internals to a customer.
	 */
	it('never leaks our internals into user-facing copy', () => {
		const forbidden = [
			'worker',
			'stderr',
			'presumed dead',
			'lease',
			'parser',
			'rls',
			'tenant',
			'qdrant',
			'postgres',
			'exit code',
			'panic'
		];

		for (const s of [...ALL_STATUSES, 'unknown' as const]) {
			const copy = `${toDisplay(s).label} ${toDisplay(s).description}`.toLowerCase();
			for (const word of forbidden) {
				expect(copy, `"${s}" copy leaks "${word}"`).not.toContain(word);
			}
		}
	});

	it('blames neither the user nor us for a `failed`, because we cannot tell which it was', () => {
		const { description } = toDisplay('failed');
		// It must hold both possibilities open: the status conflates a broken file with a dead worker.
		expect(description).toMatch(/damaged/i);
		expect(description).toMatch(/our side/i);
		// And it must give the advice that is correct under both.
		expect(description).toMatch(/again/i);
	});

	it('names the one failure we can honestly be specific about', () => {
		// `quarantined` has exactly one writer today: the oversize check. If a second ever appears,
		// this copy becomes a lie and this test is the tripwire.
		expect(toDisplay('quarantined').description).toMatch(/25 MB/);
	});

	it('does not paint an expired upload as a breakage', () => {
		// Nothing broke — the user closed a tab. Red would be a lie.
		expect(toDisplay('expired').variant).toBe('outline');
		expect(toDisplay('failed').variant).toBe('destructive');
	});
});
