/** "m@example.com" → "M"; "john.doe@acme.test" → "JD". The API gives us no display name. */
export function initialsFromEmail(email: string): string {
	const local = email.split('@')[0] ?? '';
	const parts = local.split(/[.\-_+]/).filter(Boolean);
	const initials = (parts[0]?.[0] ?? '') + (parts[1]?.[0] ?? '');
	return (initials || email[0] || '?').toUpperCase();
}
