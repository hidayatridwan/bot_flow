import { describe, expect, it } from 'vitest';
import { isPlausibleEmail, isValidSlug, passwordByteLength, signupSchema, slugify } from './schema';

/**
 * These cases are ported verbatim from the Rust unit tests in `crates/api/src/accounts.rs`
 * (`email_validation_accepts_and_rejects`, `slugify_produces_valid_slugs`) and
 * `crates/common/src/key.rs`. If this file and the Rust drift, the user sees a client-side "valid"
 * that the server 422s.
 */

describe('isPlausibleEmail — mirrors accounts::is_plausible_email', () => {
	it('accepts what the API accepts', () => {
		expect(isPlausibleEmail('owner@acme.test')).toBe(true);
		expect(isPlausibleEmail('a.b+tag@sub.example.com')).toBe(true);
	});

	it('rejects what the API rejects', () => {
		expect(isPlausibleEmail('no-at-sign')).toBe(false);
		expect(isPlausibleEmail('@acme.test')).toBe(false);
		expect(isPlausibleEmail('owner@localhost')).toBe(false); // no dot in the domain
		expect(isPlausibleEmail('owner @acme.test')).toBe(false); // whitespace
	});
});

describe('slugify — mirrors accounts::slugify', () => {
	it('produces the same slugs as the Rust', () => {
		expect(slugify('Acme Corp')).toBe('acme-corp');
		expect(slugify('  Föö & Bar!!  ')).toBe('f-bar'); // non-ascii dropped, runs collapsed, trimmed
		expect(slugify('already-good')).toBe('already-good');
		expect(slugify('!!!')).toBe(''); // pure punctuation → empty; the caller must reject
	});

	it('always emits something the tenant-slug contract accepts', () => {
		for (const name of ['Acme Corp', 'already-good', 'X']) {
			expect(isValidSlug(slugify(name))).toBe(true);
		}
	});
});

describe('isValidSlug — mirrors common::key::is_valid_slug', () => {
	it.each(['acme', 'a', 'globex-inc', 'tenant123', '0abc'])('accepts %s', (s) => {
		expect(isValidSlug(s)).toBe(true);
	});

	it.each(['ACME', '-acme', 'a_b', 'a b', 'a/b', '..', ''])('rejects %s', (s) => {
		expect(isValidSlug(s)).toBe(false);
	});

	it('rejects a slug over 63 characters', () => {
		expect(isValidSlug('a'.repeat(63))).toBe(true);
		expect(isValidSlug('a'.repeat(64))).toBe(false);
	});
});

describe('passwordByteLength — the API counts BYTES, not characters', () => {
	it('agrees with String::len for ascii', () => {
		expect(passwordByteLength('abcdefgh')).toBe(8);
	});

	it('counts a multi-byte character as more than one', () => {
		// 7 characters, but 10 bytes — the Rust would accept this and z.string().min(8) would too.
		// The point is that we agree with the Rust, whichever way it falls.
		expect(passwordByteLength('pässwör')).toBe(9);
		// 4 characters, 16 bytes: the API accepts it, so we must not reject it.
		expect(passwordByteLength('🔑🔑🔑🔑')).toBe(16);
	});
});

describe('signupSchema', () => {
	const valid = {
		name: 'Acme',
		slug: 'acme',
		email: 'owner@acme.test',
		password: 'correct horse battery staple',
		confirmPassword: 'correct horse battery staple'
	};

	it('accepts a valid signup', () => {
		expect(signupSchema.safeParse(valid).success).toBe(true);
	});

	it('rejects a password the API would reject, by byte length', () => {
		const result = signupSchema.safeParse({
			...valid,
			password: 'short',
			confirmPassword: 'short'
		});
		expect(result.success).toBe(false);
	});

	it('accepts a 4-emoji password, because the API does (16 bytes)', () => {
		expect(
			signupSchema.safeParse({ ...valid, password: '🔑🔑🔑🔑', confirmPassword: '🔑🔑🔑🔑' })
				.success
		).toBe(true);
	});

	it('puts a mismatch on confirmPassword, not password', () => {
		const result = signupSchema.safeParse({ ...valid, confirmPassword: 'different' });
		expect(result.success).toBe(false);
		if (!result.success) {
			expect(result.error.issues.map((i) => i.path.join('.'))).toContain('confirmPassword');
		}
	});
});
