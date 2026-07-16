import { describe, expect, it } from 'vitest';
import { MAX_UPLOAD_BYTES, extensionOf, validateFile } from './schema';

/**
 * The TS half of a two-language contract. `extensionOf` mirrors `common::key::extension_of`, and a
 * drift here shows the user a client-side "valid" that the API then 400s — invisible until a real
 * user hits it.
 *
 * The first block is `key.rs::only_parser_supported_extensions_pass`, ported verbatim. The second is
 * `key.rs::dotfiles_have_no_extension`, likewise. If either Rust test changes, this file changes.
 */

describe('extensionOf — ported from key.rs::only_parser_supported_extensions_pass', () => {
	it('accepts what the parser supports', () => {
		expect(extensionOf('cv.pdf')).toBe('pdf');
		expect(extensionOf('NOTES.MD')).toBe('md'); // case-insensitive
		expect(extensionOf('a.b.txt')).toBe('txt');
	});

	it('rejects what it does not', () => {
		expect(extensionOf('resume.docx')).toBeNull();
		expect(extensionOf('noext')).toBeNull();
	});
});

describe('extensionOf — ported from key.rs::dotfiles_have_no_extension', () => {
	it('treats a leading dot as a stem, not an extension', () => {
		// THE case the naive mirror gets wrong. `split('.').pop()` returns "pdf" here and waves the
		// file through; Rust's Path::extension() returns None and the API 400s it.
		expect(extensionOf('.pdf')).toBeNull();
		expect(extensionOf('.txt')).toBeNull();
	});

	it('finds a real extension once there is a second dot', () => {
		expect(extensionOf('..pdf')).toBe('pdf');
		expect(extensionOf('.hidden.md')).toBe('md');
	});

	it('treats a trailing dot as an empty extension', () => {
		expect(extensionOf('file.')).toBeNull();
		expect(extensionOf('..')).toBeNull();
	});

	it('reads only the last path segment', () => {
		expect(extensionOf('dir/file.pdf')).toBe('pdf');
		expect(extensionOf('a.pdf/b.txt')).toBe('txt');
	});
});

describe('validateFile', () => {
	it('passes a good file', () => {
		expect(validateFile({ name: 'faq.pdf', size: 1024 })).toBeNull();
	});

	it('rejects an unsupported type', () => {
		expect(validateFile({ name: 'a.docx', size: 1024 })).toMatch(/isn't supported/i);
	});

	it('rejects at the cap, and says the same thing the quarantine badge says', () => {
		expect(validateFile({ name: 'a.pdf', size: MAX_UPLOAD_BYTES })).toBeNull();
		const tooBig = validateFile({ name: 'a.pdf', size: MAX_UPLOAD_BYTES + 1 });
		// Word-identical to status.ts's `quarantined` copy. The client-side rejection and the
		// server-side quarantine are the same rule, and must never read as two.
		expect(tooBig).toBe(
			"This file is over the 25 MB limit, so we couldn't keep it. Upload a smaller file."
		);
	});

	it('rejects an empty file', () => {
		expect(validateFile({ name: 'a.pdf', size: 0 })).toMatch(/empty/i);
	});
});
