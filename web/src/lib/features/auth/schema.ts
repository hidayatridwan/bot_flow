import { z } from 'zod';

/**
 * The TypeScript mirror of the Rust validators. The API is the authority — this exists so the user
 * finds out before a round trip, not instead of one.
 *
 * A drift between this file and `crates/api/src/accounts.rs` shows the user a client-side "valid"
 * that the server then rejects with a 422, which is exactly the kind of bug nobody notices until a
 * real customer with an umlaut in their business name hits it. `schema.test.ts` ports the Rust unit
 * tests verbatim to hold the two in step.
 */

/** Mirrors `accounts::is_plausible_email`. Deliberately permissive; not an RFC validator. */
export function isPlausibleEmail(raw: string): boolean {
	const s = raw.trim();
	const at = s.indexOf('@');
	if (at < 0) return false;
	const local = s.slice(0, at);
	const domain = s.slice(at + 1);
	return (
		local.length > 0 &&
		domain.includes('.') &&
		!domain.startsWith('.') &&
		!domain.endsWith('.') &&
		!/\s/.test(s)
	);
}

export const MIN_PASSWORD_BYTES = 8;

/**
 * The Rust check is `password.len() < 8` on a String — that is BYTES, not characters.
 * `z.string().min(8)` counts UTF-16 code units, so for an 8-character string containing an emoji or
 * an accent the two disagree and the server 422s something the client called valid.
 */
export function passwordByteLength(s: string): number {
	return new TextEncoder().encode(s).length;
}

/** Mirrors `common::key::is_valid_slug` and the `tenants_id_slug` DB CHECK. */
const SLUG_RE = /^[a-z0-9][a-z0-9-]{0,62}$/;

export function isValidSlug(s: string): boolean {
	return SLUG_RE.test(s);
}

/**
 * Mirrors `accounts::slugify`. Note that non-ASCII is DROPPED, not transliterated: "Föö & Bar"
 * becomes "f-bar". We show the result live as the user types the business name, so that surprise
 * lands before submit rather than after.
 */
export function slugify(input: string): string {
	let out = '';
	let prevDash = false;
	for (const c of input.toLowerCase()) {
		if (c >= 'a' && c <= 'z') {
			out += c;
			prevDash = false;
		} else if (c >= '0' && c <= '9') {
			out += c;
			prevDash = false;
		} else if (out.length > 0 && !prevDash) {
			out += '-';
			prevDash = true;
		}
	}
	return out.replace(/^-+|-+$/g, '').slice(0, 63);
}

const email = z
	.string()
	.min(1, 'Enter your email.')
	.refine(isPlausibleEmail, 'Enter a valid email address.');

const password = z
	.string()
	.min(1, 'Enter your password.')
	.refine(
		(v) => passwordByteLength(v) >= MIN_PASSWORD_BYTES,
		`Password must be at least ${MIN_PASSWORD_BYTES} characters.`
	);

export const loginSchema = z.object({
	email,
	// No length rule on login: the password either matches what is stored or it does not, and telling
	// someone their password is "too short" to log in is noise.
	password: z.string().min(1, 'Enter your password.')
});

export const signupSchema = z
	.object({
		name: z.string().min(1, 'Enter your business name.'),
		slug: z
			.string()
			.min(1, 'Enter a workspace URL.')
			.refine(
				isValidSlug,
				'Use lowercase letters, numbers and dashes only, starting with a letter or number.'
			),
		email,
		password,
		// Client-only: `RegisterRequest` has no such field. Checked here, then dropped — it never goes
		// on the wire.
		confirmPassword: z.string().min(1, 'Confirm your password.')
	})
	.refine((v) => v.password === v.confirmPassword, {
		message: 'Passwords do not match.',
		path: ['confirmPassword']
	});

export const forgotPasswordSchema = z.object({ email });

export const resetPasswordSchema = z
	.object({
		// The token rides in a hidden field, put there by the page from the query string. Validated
		// only for presence: its shape is the API's business, and a client-side format rule here
		// would be a second definition of a credential format to drift from.
		token: z.string().min(1, 'This reset link is missing its token.'),
		password,
		// Client-only, exactly like signup's: dropped before the wire call.
		confirmPassword: z.string().min(1, 'Confirm your password.')
	})
	.refine((v) => v.password === v.confirmPassword, {
		message: 'Passwords do not match.',
		path: ['confirmPassword']
	});

export const changePasswordSchema = z
	.object({
		currentPassword: z.string().min(1, 'Enter your current password.'),
		newPassword: password,
		confirmPassword: z.string().min(1, 'Confirm your new password.')
	})
	.refine((v) => v.newPassword === v.confirmPassword, {
		message: 'Passwords do not match.',
		path: ['confirmPassword']
	});

export type LoginSchema = typeof loginSchema;
export type SignupSchema = typeof signupSchema;
export type ForgotPasswordSchema = typeof forgotPasswordSchema;
export type ResetPasswordSchema = typeof resetPasswordSchema;
export type ChangePasswordSchema = typeof changePasswordSchema;
