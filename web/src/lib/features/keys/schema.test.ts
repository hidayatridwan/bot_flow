import { describe, expect, it } from 'vitest';
import { firstInvalidOrigin, mintSchema, normalizeOrigin, parseOrigins } from './schema';

/**
 * The TS half of a two-language contract, ported verbatim from
 * `auth.rs::origins_are_canonicalised_to_what_a_browser_actually_sends` and its two siblings.
 *
 * The stakes are higher than a normal validation mirror: the API compares stored origins to the
 * `Origin` header by string equality, so a value that survives one side and not the other produces a
 * key that mints happily and 403s forever.
 */

describe('normalizeOrigin — ported from auth.rs', () => {
	it('passes through what a browser already sends', () => {
		expect(normalizeOrigin('https://acme.com')).toBe('https://acme.com');
		expect(normalizeOrigin('http://localhost:5500')).toBe('http://localhost:5500');
	});

	it('canonicalises what a human copies from an address bar', () => {
		expect(normalizeOrigin('https://acme.com/')).toBe('https://acme.com');
		expect(normalizeOrigin('https://ACME.com')).toBe('https://acme.com');
		expect(normalizeOrigin('  https://acme.com  ')).toBe('https://acme.com');
	});

	it('strips a default port and keeps a non-default one', () => {
		// A browser omits the default port, so storing it guarantees a mismatch.
		expect(normalizeOrigin('https://acme.com:443')).toBe('https://acme.com');
		expect(normalizeOrigin('http://acme.com:80')).toBe('http://acme.com');
		expect(normalizeOrigin('https://acme.com:8443')).toBe('https://acme.com:8443');
		// :443 is only default for https — over http it is a real, distinct origin.
		expect(normalizeOrigin('http://acme.com:443')).toBe('http://acme.com:443');
	});

	it('rejects what can never match', () => {
		for (const bad of [
			'',
			'   ',
			'acme.com',
			'//acme.com',
			'https://',
			'https://acme.com/path',
			'https://acme.com?a=b',
			'https://acme.com#frag',
			'https://user@acme.com',
			'https://acme.com:notaport',
			'ftp://acme.com',
			'file://acme.com'
		]) {
			expect(normalizeOrigin(bad), `should reject: ${bad}`).toBeNull();
		}
	});

	it('never allow-lists the null origin', () => {
		// file:// pages and sandboxed iframes send `Origin: null`. Allow-listing it admits all of them.
		expect(normalizeOrigin('null')).toBeNull();
		expect(normalizeOrigin('NULL')).toBeNull();
		expect(normalizeOrigin(' null ')).toBeNull();
	});

	it('is idempotent', () => {
		for (const raw of ['https://acme.com/', 'https://ACME.com:443', 'http://localhost:5500']) {
			const once = normalizeOrigin(raw)!;
			expect(normalizeOrigin(once)).toBe(once);
		}
	});
});

describe('parseOrigins', () => {
	it('reads one origin per line and ignores blanks', () => {
		expect(parseOrigins('https://a.com\n\n  https://b.com  \n')).toEqual([
			'https://a.com',
			'https://b.com'
		]);
		expect(parseOrigins('')).toEqual([]);
		expect(parseOrigins('   \n  ')).toEqual([]);
	});
});

describe('firstInvalidOrigin', () => {
	it('names the offending line', () => {
		expect(firstInvalidOrigin('https://a.com\nacme.com')).toBe('acme.com');
	});
	it('is null when every line is usable', () => {
		expect(firstInvalidOrigin('https://a.com\nhttps://b.com')).toBeNull();
		expect(firstInvalidOrigin('')).toBeNull();
	});
});

describe('mintSchema', () => {
	it('accepts a publishable key with an origin', () => {
		const r = mintSchema.safeParse({
			kind: 'publishable',
			label: 'widget',
			origins: 'https://a.com'
		});
		expect(r.success).toBe(true);
	});

	it('REJECTS a publishable key with no origins', () => {
		// The whole trap: the API mints this happily today and the key 403s forever.
		const r = mintSchema.safeParse({ kind: 'publishable', label: '', origins: '' });
		expect(r.success).toBe(false);
		expect(JSON.stringify(r.error?.issues)).toMatch(/at least one origin/i);
	});

	it('allows a secret key with no origins', () => {
		// Secret keys are never origin-checked, so an empty list is their natural state.
		const r = mintSchema.safeParse({ kind: 'secret', label: '', origins: '' });
		expect(r.success).toBe(true);
	});

	it('rejects an unusable origin on either kind', () => {
		expect(mintSchema.safeParse({ kind: 'secret', label: '', origins: 'acme.com' }).success).toBe(
			false
		);
	});
});
