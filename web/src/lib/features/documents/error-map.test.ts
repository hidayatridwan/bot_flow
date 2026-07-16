import { describe, expect, it } from 'vitest';
import type { ApiError } from '$lib/types/api';
import { mapPutError, mapUploadError } from './error-map';

const err = (over: Partial<ApiError>): ApiError => ({
	status: 500,
	message: 'boom',
	kind: 'server',
	path: '/documents/upload-url',
	...over
});

describe('mapUploadError', () => {
	it('names the rate limit', () => {
		expect(mapUploadError(err({ status: 429, kind: 'client' }))).toMatch(/wait a minute/i);
	});

	it('translates an unsupported type into the same words the client uses', () => {
		const m = mapUploadError(
			err({
				status: 400,
				kind: 'client',
				message: 'unsupported file type; expected one of pdf, txt, md'
			})
		);
		expect(m).toMatch(/isn't supported/i);
		// The raw API sentence is fine to act on but not to show — it lists extensions in a shape we
		// already say better in the UI.
		expect(m).not.toContain('expected one of');
	});

	it('never shows a 5xx body', () => {
		// Invariant 16: unexpected failures are logged in full and answered generically.
		const m = mapUploadError(err({ status: 500, kind: 'server', message: 'RLS policy denied' }));
		expect(m).not.toContain('RLS');
		expect(m).toMatch(/something went wrong/i);
	});

	it('never shows an axum extractor rejection', () => {
		const m = mapUploadError(
			err({ status: 422, kind: 'malformed', message: 'Failed to deserialize the JSON body' })
		);
		expect(m).not.toContain('deserialize');
	});

	it('distinguishes an unreachable service from a broken one', () => {
		expect(mapUploadError(err({ status: 0, kind: 'transport' }))).toMatch(/reach the service/i);
	});
});

describe('mapPutError', () => {
	it('blames the connection when there was no response', () => {
		expect(mapPutError(0)).toMatch(/connection/i);
	});

	it('stays generic for a real status', () => {
		expect(mapPutError(500)).toMatch(/try again/i);
		// Storage internals are not ours to explain either.
		expect(mapPutError(500)).not.toMatch(/minio|s3|bucket/i);
	});
});
