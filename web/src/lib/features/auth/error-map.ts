import type { NumericRange } from '@sveltejs/kit';
import type { ApiError } from '$lib/types/api';
import { GENERIC, RATE_LIMITED, UNREACHABLE } from '$lib/utils/api-copy';

/** Every field an auth error can land on. `null` means the form itself, not an input. */
export type AuthField = 'name' | 'slug' | 'email' | 'password';

export interface MappedError {
	field: AuthField | null;
	message: string;
}

/** `fail()` needs a 4xx/5xx. A transport failure has status 0, which is not one. */
export function toFailStatus(error: ApiError): NumericRange<400, 599> {
	const status = error.status >= 400 && error.status <= 599 ? error.status : 503;
	return status as NumericRange<400, 599>;
}

export function mapLoginError(error: ApiError): MappedError {
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

export function mapRegisterError(error: ApiError): MappedError {
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
