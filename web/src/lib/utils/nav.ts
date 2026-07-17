/**
 * Is a nav item's target the page we are currently on?
 *
 * Most of the sidebar is still mock data pointing at `#`. Comparing those as if they were paths is
 * how every mock item lights up at once, so a target that is not an absolute path is never current.
 */
export function isCurrentPath(url: string, pathname: string): boolean {
	if (!url.startsWith('/')) return false;
	// The second half is for nested routes — `/documents/abc` should still light `Documents`. Guarded
	// by the `/` so `/doc` cannot claim `/documents`.
	return pathname === url || pathname.startsWith(`${url}/`);
}
