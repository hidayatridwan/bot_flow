import { describe, expect, it } from 'vitest';
import type { ApiError } from '$lib/types/api';
import { mapLoginError, mapRegisterError, toFailStatus } from './error-map';

const error = (status: number, message: string, kind: ApiError['kind'] = 'client'): ApiError => ({
	status,
	message,
	kind,
	path: '/auth/test'
});

describe('mapLoginError — the non-oracle rule', () => {
	/**
	 * The most important assertion in the web app. The API returns one identical message for an
	 * unknown email and a wrong password so that it cannot be used to enumerate accounts. If this
	 * ever maps to a field, the UI rebuilds that oracle: under Email it reads "this email is wrong",
	 * under Password it reads "this email exists, but the password is wrong".
	 */
	it('puts a 401 on the form, never on a field', () => {
		const mapped = mapLoginError(error(401, 'invalid email or password'));
		expect(mapped.field).toBeNull();
		expect(mapped.message).toBe('Invalid email or password.');
	});

	it('puts a 429 on the form', () => {
		expect(mapLoginError(error(429, 'rate limit exceeded, slow down')).field).toBeNull();
	});

	it('never surfaces an internal message', () => {
		const mapped = mapLoginError(error(500, 'internal server error', 'server'));
		expect(mapped.field).toBeNull();
		expect(mapped.message).not.toContain('internal');
	});

	it('never surfaces an axum extractor rejection', () => {
		const mapped = mapLoginError(
			error(422, 'Failed to deserialize the JSON body into the target type', 'malformed')
		);
		expect(mapped.message).not.toContain('deserialize');
	});
});

describe('mapRegisterError — the two 409s share a status and must not share a field', () => {
	it('sends a taken email to the email field', () => {
		const mapped = mapRegisterError(error(409, 'an account with this email already exists'));
		expect(mapped.field).toBe('email');
	});

	it('sends a taken tenant to the slug field', () => {
		const mapped = mapRegisterError(error(409, 'tenant already exists'));
		expect(mapped.field).toBe('slug');
	});

	it('maps the 400 slug-format error to the slug field', () => {
		const mapped = mapRegisterError(error(400, 'tenant id must match ^[a-z0-9][a-z0-9-]{0,62}$'));
		expect(mapped.field).toBe('slug');
	});

	it('maps the 422s to their fields', () => {
		expect(mapRegisterError(error(422, 'email is not a valid address')).field).toBe('email');
		expect(mapRegisterError(error(422, 'password must be at least 8 characters')).field).toBe(
			'password'
		);
		expect(
			mapRegisterError(
				error(422, "could not derive a tenant slug from tenant_name; provide 'slug'")
			).field
		).toBe('slug');
	});

	it('falls back to a form-level generic message for anything unrecognised', () => {
		const mapped = mapRegisterError(error(409, 'some new conflict we have never seen'));
		expect(mapped.field).toBeNull();
	});
});

describe('toFailStatus', () => {
	it('passes a real http status through', () => {
		expect(toFailStatus(error(409, 'x'))).toBe(409);
	});

	it('turns a transport failure (status 0) into something fail() accepts', () => {
		expect(toFailStatus(error(0, 'could not reach the api', 'transport'))).toBe(503);
	});
});
