import { describe, expect, it } from 'vitest';
import { breadcrumbsFor } from './breadcrumb';

describe('breadcrumbsFor', () => {
	it('names the page you are actually on', () => {
		// It used to render "Build Your Application / Data Fetching" on every page, linking `##`.
		expect(breadcrumbsFor('/documents')).toEqual([{ label: 'Documents', href: null }]);
		expect(breadcrumbsFor('/keys')).toEqual([{ label: 'API keys', href: null }]);
	});

	it('never links the final crumb', () => {
		// The page you are on is not a link to itself.
		const crumbs = breadcrumbsFor('/settings/password');
		expect(crumbs.at(-1)).toEqual({ label: 'Password', href: null });
	});

	it('does not link an intermediate segment that has no route', () => {
		// **The assertion that matters.** `/settings/password` exists; `/settings` does not. Linking
		// every prefix — the obvious implementation — puts a guaranteed 404 inside the one component
		// whose job is orientation.
		const [settings] = breadcrumbsFor('/settings/password');
		expect(settings).toEqual({ label: 'Settings', href: null });
	});

	it('links an intermediate segment that is a real route', () => {
		const [documents, id] = breadcrumbsFor('/documents/abc');
		expect(documents).toEqual({ label: 'Documents', href: '/documents' });
		expect(id.href).toBeNull();
	});

	it('renders nothing at the root', () => {
		// A single crumb reading "Home" is decoration, not navigation.
		expect(breadcrumbsFor('/')).toEqual([]);
		expect(breadcrumbsFor('')).toEqual([]);
	});

	it('falls back to a readable label for an unnamed segment', () => {
		// A new route gets a correct breadcrumb by existing; it is never rendered as a raw slug.
		expect(breadcrumbsFor('/upload-url')).toEqual([{ label: 'Upload url', href: null }]);
	});

	it('tolerates a trailing slash', () => {
		expect(breadcrumbsFor('/documents/')).toEqual([{ label: 'Documents', href: null }]);
	});
});
