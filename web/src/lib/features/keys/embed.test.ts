import { describe, expect, it } from 'vitest';
import { NotAPublishableKeyError, PUBLIC_KEY_PLACEHOLDER, embedSnippet } from './embed';

const PK = 'pk_' + 'a'.repeat(64);
const SK = 'sk_' + 'b'.repeat(64);

describe('embedSnippet — it must never carry a secret key', () => {
	/**
	 * The most important assertion in this slice. The snippet's entire purpose is to be pasted into a
	 * public page. An `sk_` there is invariant 15 inverted: the key that may do everything, printed
	 * where anyone can read it. Refusing is the only safe behaviour — a caller that hands this an sk_
	 * has a bug, and silently rendering it would publish the tenant's credentials.
	 */
	it('throws on a secret key rather than emitting one', () => {
		expect(() => embedSnippet({ apiBase: 'https://api.example.com', publicKey: SK })).toThrow(
			NotAPublishableKeyError
		);
	});

	it('throws on anything that is not a pk_', () => {
		for (const bad of ['sess_abc', 'abc', '', 'PK_abc']) {
			expect(() => embedSnippet({ apiBase: 'https://api.example.com', publicKey: bad })).toThrow();
		}
	});

	it('allows the placeholder, for the template shown when no raw key exists', () => {
		const s = embedSnippet({
			apiBase: 'https://api.example.com',
			publicKey: PUBLIC_KEY_PLACEHOLDER
		});
		expect(s).toContain(PUBLIC_KEY_PLACEHOLDER);
	});
});

describe('embedSnippet — the output', () => {
	it('carries the key and the api base', () => {
		const s = embedSnippet({ apiBase: 'https://api.example.com', publicKey: PK });
		expect(s).toContain(PK);
		expect(s).toContain('https://api.example.com');
		expect(s).toContain('ChatWidget.init');
		expect(s).toContain('widget.js');
	});

	it('strips a trailing slash from the api base', () => {
		// widget.js does `config.apiBase.replace(/\/$/, '')` itself, but emitting `//ask/stream` in the
		// snippet would still look broken to whoever reads it.
		const s = embedSnippet({ apiBase: 'https://api.example.com/', publicKey: PK });
		expect(s).toContain('"https://api.example.com"');
		expect(s).not.toContain('example.com/"');
	});

	it('includes a title only when given one', () => {
		expect(embedSnippet({ apiBase: 'https://a.com', publicKey: PK })).not.toContain('title:');
		expect(embedSnippet({ apiBase: 'https://a.com', publicKey: PK, title: 'Acme Help' })).toContain(
			'title: "Acme Help"'
		);
	});

	it('escapes a title that would otherwise break out of the string', () => {
		// The title is tenant-controlled and lands inside a <script> the tenant pastes elsewhere.
		const s = embedSnippet({ apiBase: 'https://a.com', publicKey: PK, title: 'He said "hi"' });
		expect(s).toContain('"He said \\"hi\\""');
	});
});
