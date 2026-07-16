import { describe, expect, it } from 'vitest';
import { formatCreatedAt } from './format';

describe('formatCreatedAt', () => {
	it('parses what Postgres actually sends', () => {
		// The real shape observed from `created_at::text` — a space separator and a `+00` offset.
		// `new Date()` rejects it outright, which is how the raw string reached the UI.
		const out = formatCreatedAt('2026-07-16 11:39:20.470205+00', 'en-US');
		expect(out).not.toContain('Invalid');
		expect(out).not.toBe('2026-07-16 11:39:20.470205+00');
		expect(out).toMatch(/2026/);
	});

	it('handles the offset that Date.parse rejects', () => {
		// The whole bug in one assertion: this is valid Postgres and invalid ISO 8601.
		expect(new Date('2026-07-16T11:39:20.470205+00').getTime()).toBeNaN();
		expect(formatCreatedAt('2026-07-16 11:39:20.470205+00', 'en-US')).toMatch(/2026/);
	});

	it('accepts a non-zero offset too', () => {
		expect(formatCreatedAt('2026-07-16 11:39:20+07', 'en-US')).toMatch(/2026/);
		expect(formatCreatedAt('2026-07-16 11:39:20-05', 'en-US')).toMatch(/2026/);
	});

	it('still handles a proper ISO string, in case the API is ever fixed', () => {
		expect(formatCreatedAt('2026-07-16T11:39:20.470Z', 'en-US')).toMatch(/2026/);
		expect(formatCreatedAt('2026-07-16T11:39:20+00:00', 'en-US')).toMatch(/2026/);
	});

	it('falls back to the raw value rather than rendering "Invalid Date"', () => {
		expect(formatCreatedAt('not a date at all')).toBe('not a date at all');
		expect(formatCreatedAt('')).toBe('');
	});
});
