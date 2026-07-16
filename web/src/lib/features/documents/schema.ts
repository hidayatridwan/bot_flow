/**
 * The TS mirror of the Rust upload validators. Drift here shows the user a client-side "valid" that
 * the API then rejects — the same trap `features/auth/schema.ts` documents for the password rule.
 *
 * The API is the authority. These checks exist to fail fast and to avoid creating a document row for
 * a file we already know is unusable; they are never the enforcement.
 */

/** Mirrors `common::key::ALLOWED_EXTENSIONS`. The sidecar supports exactly these. */
export const ALLOWED_EXTENSIONS = ['pdf', 'txt', 'md'] as const;

/**
 * Mirrors `MAX_UPLOAD_BYTES` in `crates/worker/src/main.rs`.
 *
 * Duplicated, and it can drift: the cap is read from the *worker's* env, which the BFF cannot see, so
 * an operator who raises it leaves this number lying. The API does not expose it. Keep the copy and
 * the wording in step with `status.ts`'s `quarantined` copy — a user must not read the client-side
 * rejection and the server-side quarantine as two different rules.
 */
export const MAX_UPLOAD_BYTES = 25 * 1024 * 1024;

/**
 * Mirrors `common::key::extension_of`, which is `Path::extension()` — and that is *not*
 * `split('.').pop()`.
 *
 * Rust treats a leading dot as part of the stem, so `.pdf` has NO extension and must be rejected;
 * `..pdf` does have one. Pinned on both sides: `key.rs`'s `dotfiles_have_no_extension` and
 * `schema.test.ts`. Get this wrong and the client waves `.pdf` through for the API to 400.
 */
export function extensionOf(filename: string): string | null {
	// Path::file_name() first — only the last segment counts.
	const name = filename.split('/').filter(Boolean).pop();
	if (!name) return null;

	const dot = name.lastIndexOf('.');
	// `dot <= 0` is the whole Rust rule: no dot at all, or a dot at index 0 (a dotfile, whose stem
	// is the entire name). Both are `None` there.
	if (dot <= 0) return null;

	const ext = name.slice(dot + 1).toLowerCase();
	return (ALLOWED_EXTENSIONS as readonly string[]).includes(ext) ? ext : null;
}

/** The user-facing rejection, or null if the file is worth attempting. */
export function validateFile(file: { name: string; size: number }): string | null {
	if (!extensionOf(file.name)) {
		return "That file type isn't supported. Upload a PDF, TXT, or MD file.";
	}
	if (file.size > MAX_UPLOAD_BYTES) {
		// Word-identical to status.ts's `quarantined` copy: one rule, stated once.
		return "This file is over the 25 MB limit, so we couldn't keep it. Upload a smaller file.";
	}
	if (file.size === 0) {
		return 'That file is empty.';
	}
	return null;
}
