import type { NumericRange } from '@sveltejs/kit';
import type { ApiError } from '$lib/types/api';
import { GENERIC, RATE_LIMITED, UNREACHABLE } from '$lib/utils/api-copy';

/** Every field an auth error can land on, across all the auth forms. */
export type AuthField = 'name' | 'slug' | 'email' | 'password' | 'currentPassword' | 'newPassword';

/**
 * A mapped error, parameterised by the fields *its own form* actually has.
 *
 * The parameter is not ceremony. A single shared union would let `mapChangePasswordError` return
 * `field: 'slug'` and typecheck, and `setError` would then throw at runtime on a form with no such
 * field — a 500 on the password page, discovered by a user. Narrowing per mapper makes that a
 * compile error instead, and it caught one while this was being written.
 *
 * `null` means the form itself rather than an input, which is the only correct answer whenever the
 * API deliberately withholds detail (see `mapLoginError` and `mapForgotPasswordError`).
 */
export interface MappedError<F extends AuthField = AuthField> {
	field: F | null;
	message: string;
}

/** `fail()` needs a 4xx/5xx. A transport failure has status 0, which is not one. */
export function toFailStatus(error: ApiError): NumericRange<400, 599> {
	const status = error.status >= 400 && error.status <= 599 ? error.status : 503;
	return status as NumericRange<400, 599>;
}

export function mapLoginError(error: ApiError): MappedError<never> {
	if (error.status === 429) return { field: null, message: RATE_LIMITED };

	// NEVER attach this to a field.
	//
	// The API returns the identical "invalid email or password" for an unknown email and for a wrong
	// password, deliberately, so that it is not an oracle for which emails are registered. Rendering
	// that message under Email reads as "this email is wrong"; under Password it reads as "this email
	// exists, but the password is wrong". Either one rebuilds, in the UI, the exact oracle the API
	// went out of its way to destroy.
	if (error.status === 401) return { field: null, message: 'Invalid email or password.' };

	if (error.kind === 'transport') return { field: null, message: UNREACHABLE };
	return { field: null, message: GENERIC };
}

export function mapRegisterError(error: ApiError): MappedError<'email' | 'slug' | 'password'> {
	if (error.status === 429) return { field: null, message: RATE_LIMITED };
	if (error.kind === 'transport') return { field: null, message: UNREACHABLE };

	if (error.kind === 'client') {
		const m = error.message;

		// Both of these are 409. The message is the ONLY thing that tells them apart, and they belong
		// under different inputs — matching on the status alone would put the error on the wrong field.
		// `includes`, not `===`, so a punctuation tweak in Rust degrades to the generic message rather
		// than throwing.
		if (m.includes('account with this email already exists')) {
			return { field: 'email', message: 'An account with this email already exists.' };
		}
		if (m.includes('tenant already exists')) {
			return { field: 'slug', message: 'That workspace URL is already taken. Try another.' };
		}

		if (m.includes('tenant id must match')) {
			return {
				field: 'slug',
				message: 'Use lowercase letters, numbers and dashes only, starting with a letter or number.'
			};
		}
		if (m.includes('could not derive a tenant slug')) {
			return { field: 'slug', message: 'Enter a workspace URL.' };
		}
		if (m.includes('email is not a valid address')) {
			return { field: 'email', message: 'Enter a valid email address.' };
		}
		if (m.includes('password must be at least')) {
			return { field: 'password', message: 'Password must be at least 8 characters.' };
		}
	}

	// 5xx, and 'malformed' (an axum extractor rejection). The raw text is logged by the client; it is
	// never shown, because it describes our internals rather than the caller's mistake.
	return { field: null, message: GENERIC };
}

/**
 * Forgot-password failures. **Never field-level, and never specific about the address.**
 *
 * The API answers `202` for a registered address, an unregistered one, and outright garbage — the
 * same non-oracle rule as login (invariant 18). So the only errors that can reach here are a 429 or
 * an outage, and neither says anything about the email. A branch that put a message under the Email
 * field would be inventing a signal the API refused to send.
 */
export function mapForgotPasswordError(error: ApiError): MappedError<never> {
	if (error.status === 429) return { field: null, message: RATE_LIMITED };
	if (error.kind === 'transport') return { field: null, message: UNREACHABLE };
	return { field: null, message: GENERIC };
}

/**
 * Reset-link failures.
 *
 * The 400 is deliberately one message for three causes — expired, already used, never existed —
 * because the API cannot tell them apart either (one `UPDATE … WHERE used_at IS NULL AND
 * expires_at > now()` covers all three, on purpose). Guessing which one it was would be inventing
 * detail, and "your link is invalid" plus a way to request another is the same advice regardless.
 */
export function mapResetPasswordError(error: ApiError): MappedError<'password'> {
	if (error.status === 429) return { field: null, message: RATE_LIMITED };
	if (error.kind === 'transport') return { field: null, message: UNREACHABLE };

	if (error.status === 400) {
		return {
			field: null,
			message: 'This reset link is invalid or has expired. Request a new one.'
		};
	}
	if (error.status === 422 && error.message.includes('password must be at least')) {
		return { field: 'password', message: 'Password must be at least 8 characters.' };
	}
	return { field: null, message: GENERIC };
}

/**
 * Change-password failures.
 *
 * **The 403 is the whole point of this mapper.** The API returns 403 rather than 401 for a wrong
 * current password precisely so the web app does not read it as "your session expired" and log the
 * user out for a typo — `hooks.server.ts` clears the cookie on a 401 only (invariant 21). Mapping
 * it back onto the session would undo that at the last step.
 */
export function mapChangePasswordError(
	error: ApiError
): MappedError<'currentPassword' | 'newPassword'> {
	if (error.status === 429) return { field: null, message: RATE_LIMITED };
	if (error.kind === 'transport') return { field: null, message: UNREACHABLE };

	if (error.status === 403) {
		return { field: 'currentPassword', message: 'That is not your current password.' };
	}
	if (error.status === 422 && error.message.includes('password must be at least')) {
		return { field: 'newPassword', message: 'Password must be at least 8 characters.' };
	}
	return { field: null, message: GENERIC };
}
