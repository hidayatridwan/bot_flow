import { describe, expect, it, vi } from 'vitest';
import { listDocuments } from './documents';
import type { ApiClient } from './client';

/**
 * These tests are about one character.
 *
 * The pagination cursor carries a UTC offset — `+00` — and in a query string a bare `+` decodes to
 * a **space**. So a hand-built URL sends the API a timestamp with a space where the offset should
 * be, which is rejected as an invalid cursor, and the tenant simply cannot reach page two. The
 * damage never appears on page one, which is where anyone would look.
 */

/** A client that records the path it was asked for and returns nothing useful. */
function spyClient() {
	const get = vi.fn().mockResolvedValue({ ok: true, data: { documents: [], next_cursor: null } });
	return { client: { get } as unknown as ApiClient, get };
}

const CURSOR = '2026-07-20T04:09:35.353682+00~5d2810fc-4117-4b34-b4a4-37009bffee40';

describe('listDocuments — the cursor must survive the query string', () => {
	it('asks for the bare path when there is no cursor', () => {
		const { client, get } = spyClient();
		listDocuments(client);
		// Not `/documents?` — a trailing empty query is a different URL and reads as a bug later.
		expect(get).toHaveBeenCalledWith('/documents');
	});

	it('percent-encodes the offset rather than shipping a bare +', () => {
		const { client, get } = spyClient();
		listDocuments(client, { before: CURSOR });

		const path = get.mock.calls[0][0] as string;
		// The literal `+` must not survive into the query string.
		expect(path).not.toMatch(/\+00/);
		expect(path).toContain('%2B00');
	});

	it('round-trips the cursor exactly as the API would read it', () => {
		const { client, get } = spyClient();
		listDocuments(client, { before: CURSOR });

		const path = get.mock.calls[0][0] as string;
		// Parse it back the way a server does. This is the assertion that would have caught a
		// hand-concatenated query: `new URLSearchParams` decodes `+` to a space.
		const decoded = new URLSearchParams(path.slice(path.indexOf('?') + 1)).get('before');
		expect(decoded).toBe(CURSOR);
	});

	it('treats null and undefined as "first page", not as a cursor', () => {
		// `?before=null` is a string the API would reject as an invalid cursor — a 422 on the
		// ordinary first load, for a tenant who did nothing unusual.
		for (const before of [null, undefined, '']) {
			const { client, get } = spyClient();
			listDocuments(client, { before });
			expect(get, `before=${String(before)}`).toHaveBeenCalledWith('/documents');
		}
	});
});
