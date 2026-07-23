/**
 * The current page's breadcrumb trail, derived from the URL.
 *
 * It used to be the shadcn demo text — *"Build Your Application / Data Fetching"*, with `href="##"`,
 * rendered identically on every authenticated page regardless of where you were. Two separate
 * problems: it named pages that do not exist, and it was useless for orientation, which is the only
 * job a breadcrumb has.
 *
 * Derived rather than declared, so it cannot drift from the routes: a new page gets a correct
 * breadcrumb by existing. The cost is that a segment with no entry in [`LABELS`] falls back to its
 * own slug, which is why the fallback is title-cased rather than shown raw.
 */

export interface Crumb {
	readonly label: string;
	/** `null` for the final crumb — the page you are on is not a link to itself. */
	readonly href: string | null;
}

/**
 * Segment → label, for the segments whose slug is not what a human would call the page.
 *
 * Only entries that differ from the title-cased slug earn a line here; `documents` → "Documents"
 * needs no rule.
 */
const LABELS: Record<string, string> = {
	keys: 'API keys',
	playground: 'Playground',
	settings: 'Settings',
	password: 'Password',
	dashboard: 'Dashboard',
	documents: 'Documents'
};

function labelFor(segment: string): string {
	if (LABELS[segment]) return LABELS[segment];
	// `upload-url` → "Upload url". Not pretty, but honest about being unnamed — and it means a new
	// route is never rendered as a raw slug.
	const spaced = segment.replace(/-/g, ' ');
	return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

/**
 * Paths that are real, navigable routes.
 *
 * **An intermediate segment is not automatically a page.** `/settings/password` exists;
 * `/settings` does not. Linking every prefix — the obvious implementation — puts a guaranteed 404
 * inside the one component whose entire job is orientation. So a crumb is a link only if its path is
 * listed here, and everything else renders as plain text.
 *
 * Listing them by hand is the cost of that guarantee, and the list is short because the app is. If
 * it ever falls out of step the failure is mild in the safe direction: a real page rendered as text
 * rather than a dead link offered as navigation.
 */
const LINKABLE = new Set(['/dashboard', '/documents', '/playground', '/keys']);

/**
 * Build the trail for a pathname.
 *
 * Returns `[]` for `/`, which renders nothing — a single crumb reading "Home" is decoration, not
 * navigation.
 */
export function breadcrumbsFor(pathname: string): Crumb[] {
	const segments = pathname.split('/').filter(Boolean);
	if (segments.length === 0) return [];

	return segments.map((segment, i) => {
		const path = `/${segments.slice(0, i + 1).join('/')}`;
		const isLast = i === segments.length - 1;
		return {
			label: labelFor(segment),
			href: !isLast && LINKABLE.has(path) ? path : null
		};
	});
}
