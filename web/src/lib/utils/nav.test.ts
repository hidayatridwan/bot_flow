import { describe, expect, it } from 'vitest';
import { isCurrentPath } from './nav';

describe('isCurrentPath', () => {
	it('matches the exact route', () => {
		expect(isCurrentPath('/documents', '/documents')).toBe(true);
		expect(isCurrentPath('/dashboard', '/dashboard')).toBe(true);
	});

	it('does not match a different route', () => {
		expect(isCurrentPath('/dashboard', '/documents')).toBe(false);
	});

	it('matches a nested route', () => {
		// A future /documents/{id} page should still light "Documents".
		expect(isCurrentPath('/documents', '/documents/abc-123')).toBe(true);
	});

	it('does not let a prefix claim a longer sibling', () => {
		// The `/` guard: without it, startsWith would make `/doc` claim `/documents`.
		expect(isCurrentPath('/doc', '/documents')).toBe(false);
		expect(isCurrentPath('/document', '/documents')).toBe(false);
	});

	it('never matches a mock target', () => {
		// Most of the sidebar is still `#`. Without the absolute-path guard these compare as paths,
		// and every mock item in the sidebar highlights at once.
		expect(isCurrentPath('#', '/documents')).toBe(false);
		expect(isCurrentPath('#', '#')).toBe(false);
		expect(isCurrentPath('', '/documents')).toBe(false);
		expect(isCurrentPath('https://example.com/documents', '/documents')).toBe(false);
	});
});
