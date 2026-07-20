import { describe, expect, it } from 'vitest';
import {
	ALL_FAILURE_REASONS,
	ALL_STATUSES,
	isTransient,
	toDisplay,
	toFailureReason,
	toStatus
} from './status';

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

		// Every status, and every failure reason — the reason branches are new copy, and they are the
		// ones written closest to the raw error, so they are the likeliest to leak.
		const cases: [string, ReturnType<typeof toDisplay>][] = [
			...[...ALL_STATUSES, 'unknown' as const].map(
				(s) => [s, toDisplay(s)] as [string, ReturnType<typeof toDisplay>]
			),
			...ALL_FAILURE_REASONS.map(
				(r) => [`failed/${r}`, toDisplay('failed', r)] as [string, ReturnType<typeof toDisplay>]
			)
		];

		for (const [name, d] of cases) {
			const copy = `${d.label} ${d.description}`.toLowerCase();
			for (const word of forbidden) {
				expect(copy, `"${name}" copy leaks "${word}"`).not.toContain(word);
			}
		}
	});

	it('holds both causes open when the reason is unknown', () => {
		// `null` means the row failed before the worker classified failures. Nothing recorded a cause,
		// so the copy must say only what is actually known — the pre-classification wording.
		const { description } = toDisplay('failed', null);
		expect(description).toMatch(/damaged/i);
		expect(description).toMatch(/our side/i);
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

/**
 * The point of the whole feature: a `failed` document must tell the tenant whether to re-upload or
 * to wait. These tests pin the *decision*, not the wording — each asserts on the presence or
 * absence of a call to action rather than on a sentence.
 */
describe('toDisplay(failed, reason) — re-upload, or wait', () => {
	it('gives every classified reason its own copy', () => {
		const seen = new Set(ALL_FAILURE_REASONS.map((r) => toDisplay('failed', r).description));
		expect(
			seen.size,
			'two reasons share copy — one of them is telling the tenant the wrong thing'
		).toBe(ALL_FAILURE_REASONS.length);
	});

	it('tells the tenant to act only when acting can help', () => {
		for (const r of ['unreadable_file', 'unsupported_type', 'too_large'] as const) {
			expect(
				toDisplay('failed', r).description,
				`${r} is the tenant's to fix, so the copy must ask them to`
			).toMatch(/upload|export|convert/i);
		}
	});

	it('never tells the tenant to re-upload when the fault is ours', () => {
		// The single most important assertion in this file. `system_error` covers a dead worker, a
		// crashed sidecar, an embedding outage — the document is very likely fine. Asking for a
		// re-upload sends them round a loop that cannot succeed, and blames them for our outage.
		// That was the pre-classification behaviour, and it is exactly what this feature exists to end.
		const { description } = toDisplay('failed', 'system_error');
		expect(description).not.toMatch(/upload/i);
		expect(description).toMatch(/our side|on our end/i);
	});

	it('does not blame the document when the fault is ours', () => {
		expect(toDisplay('failed', 'system_error').description).not.toMatch(/damaged|corrupt|broken/i);
	});
});

describe('toFailureReason — the wire is untrusted here too', () => {
	it('narrows every real reason', () => {
		for (const r of ALL_FAILURE_REASONS) expect(toFailureReason(r)).toBe(r);
	});

	it('degrades an unrecognised reason to null, not to a guess', () => {
		// Note this differs from `toStatus`, which has an 'unknown' branch. A reason we do not
		// recognise must fall back to the cause-agnostic copy: guessing would either blame the tenant
		// for our outage or excuse a genuinely broken file.
		expect(toFailureReason('something_new')).toBeNull();
		expect(toFailureReason('SYSTEM_ERROR')).toBeNull(); // case matters; the API is lowercase
		expect(toFailureReason('')).toBeNull();
		expect(toFailureReason(null)).toBeNull();
		expect(toFailureReason(undefined)).toBeNull();
	});
});
