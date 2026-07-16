import type { ApiError } from '$lib/types/api';
import { RATE_LIMITED, genericMessage } from '$lib/utils/api-copy';

/**
 * Upload failures → what the user is told. Two distinct surfaces:
 * `mapUploadError` covers our own BFF/API, `mapPutError` covers MinIO, which we do not control.
 */

/** A mint (`POST /documents/upload-url`) that came back 4xx/5xx. */
export function mapUploadError(error: ApiError): string {
	if (error.status === 429) return RATE_LIMITED;

	if (error.kind === 'client' && error.message.includes('unsupported file type')) {
		// The client mirrors `extension_of`, so this should be unreachable. It is here because the
		// mirror can drift, and a drifted mirror should still say something true.
		return "That file type isn't supported. Upload a PDF, TXT, or MD file.";
	}

	return genericMessage(error);
}

/**
 * The PUT to storage. A different host, different rules, and no JSON envelope — so this maps a bare
 * status, not an `ApiError`.
 *
 * 403 is handled by the caller (re-mint and retry once), not here: it means the signature lapsed,
 * which is recoverable rather than something to report.
 */
export function mapPutError(status: number): string {
	if (status === 0) return 'Upload failed. Check your connection and try again.';
	return 'Upload failed. Please try again.';
}
