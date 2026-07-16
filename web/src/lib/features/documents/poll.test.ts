import { describe, expect, it } from 'vitest';
import { POLL_MAX_MS, POLL_MIN_MS, nextInterval } from './poll';

describe('nextInterval', () => {
	it('backs off while nothing changes', () => {
		const seen: number[] = [];
		let ms = POLL_MIN_MS;
		for (let i = 0; i < 6; i++) {
			seen.push(ms);
			ms = nextInterval(ms, false);
		}
		expect(seen).toEqual([3000, 4500, 6750, 10125, 15000, 15000]);
	});

	it('snaps back to responsive the moment something changes', () => {
		expect(nextInterval(POLL_MAX_MS, true)).toBe(POLL_MIN_MS);
		expect(nextInterval(9999, true)).toBe(POLL_MIN_MS);
	});

	it('never drops below the floor', () => {
		expect(nextInterval(0, false)).toBe(POLL_MIN_MS);
		expect(nextInterval(-1000, false)).toBe(POLL_MIN_MS);
		expect(nextInterval(10, false)).toBe(POLL_MIN_MS);
	});

	it('never climbs above the ceiling', () => {
		// The ceiling is what bounds an abandoned `uploading` row, which sits for ~20 minutes before
		// the reaper settles it. Without it that is ~400 identical responses over an unpaginated table.
		expect(nextInterval(POLL_MAX_MS, false)).toBe(POLL_MAX_MS);
		expect(nextInterval(1_000_000, false)).toBe(POLL_MAX_MS);
	});

	it('reaches the ceiling within about half a minute of a stall', () => {
		let ms = POLL_MIN_MS;
		let elapsed = 0;
		let ticks = 0;
		while (ms < POLL_MAX_MS) {
			elapsed += ms;
			ms = nextInterval(ms, false);
			ticks++;
		}
		expect(ticks).toBe(4);
		expect(elapsed).toBeLessThan(25_000);
	});
});
