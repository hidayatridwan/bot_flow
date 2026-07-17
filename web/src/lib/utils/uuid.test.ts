import { describe, expect, it } from 'vitest';
import { isUuid } from './uuid';

describe('isUuid', () => {
	it('accepts what the API mints', () => {
		// Real ids captured from POST /ask and POST /documents/upload-url.
		expect(isUuid('e4490fbb-e9ca-4c8b-845c-fb39f31ae699')).toBe(true);
		expect(isUuid('7fe5c124-0b2c-4c9e-9f3a-1d2e3f4a5b6c')).toBe(true);
	});

	it('is case-insensitive, because Postgres and a hand-pasted id disagree on case', () => {
		expect(isUuid('E4490FBB-E9CA-4C8B-845C-FB39F31AE699')).toBe(true);
	});

	it('rejects the shapes that would otherwise reach the API', () => {
		expect(isUuid('')).toBe(false);
		expect(isUuid('not-a-uuid')).toBe(false);
		expect(isUuid('e4490fbb-e9ca-4c8b-845c')).toBe(false); // truncated
		expect(isUuid('e4490fbbe9ca4c8b845cfb39f31ae699')).toBe(false); // unhyphenated
		expect(isUuid('e4490fbb-e9ca-4c8b-845c-fb39f31ae699x')).toBe(false); // trailing junk
		expect(isUuid(' e4490fbb-e9ca-4c8b-845c-fb39f31ae699')).toBe(false); // leading space
		expect(isUuid('g4490fbb-e9ca-4c8b-845c-fb39f31ae699')).toBe(false); // g is not hex
	});

	it('rejects a path traversal dressed as an id', () => {
		// The reason this guards a relay: `/documents/${id}/upload-url` interpolates it.
		expect(isUuid('../../admin/tenants')).toBe(false);
		expect(isUuid('e4490fbb-e9ca-4c8b-845c-fb39f31ae699/../x')).toBe(false);
	});

	it('rejects non-strings without throwing', () => {
		expect(isUuid(undefined)).toBe(false);
		expect(isUuid(null)).toBe(false);
		expect(isUuid(42)).toBe(false);
		expect(isUuid({})).toBe(false);
		// An array whose toString() looks right must not sneak through a loose comparison.
		expect(isUuid(['e4490fbb-e9ca-4c8b-845c-fb39f31ae699'])).toBe(false);
	});
});
