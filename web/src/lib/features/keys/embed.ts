/**
 * The snippet a tenant pastes into their site.
 *
 * Pure and tested, because the thing it must never do is silently correct: emit an `sk_`. A secret key
 * in public page source is invariant 15 inverted — the key that "may do everything", printed on a page
 * anyone can view.
 */

export interface EmbedOptions {
	/** The API URL the *visitor's* browser will reach — not necessarily the BFF's own API_BASE_URL. */
	apiBase: string;
	publicKey: string;
	title?: string;
}

/** The placeholder shown on the Keys page, where no raw key exists any more. */
export const PUBLIC_KEY_PLACEHOLDER = 'pk_your_publishable_key';

export class NotAPublishableKeyError extends Error {}

/**
 * Build the `<script>` block. Throws if handed anything but a `pk_` (or the placeholder): this is the
 * one function whose output is designed to be pasted somewhere public, so it refuses rather than
 * trusting its caller.
 */
export function embedSnippet({ apiBase, publicKey, title }: EmbedOptions): string {
	if (publicKey !== PUBLIC_KEY_PLACEHOLDER && !publicKey.startsWith('pk_')) {
		throw new NotAPublishableKeyError('the embed snippet only ever carries a publishable key');
	}

	const base = apiBase.replace(/\/$/, '');
	const titleLine = title ? `\n    title: ${JSON.stringify(title)},` : '';

	// The `src` is the API's own `/widget.js` (phase 7), so the snippet is copy-pasteable as-is —
	// no `/path/to/` for the tenant to resolve, and the widget updates when the API does. It is the
	// same origin the widget then calls, which is the point: one place to configure, not two.
	return `<script src="${base}/widget.js"></script>
<script>
  ChatWidget.init({
    apiBase: ${JSON.stringify(base)},
    publicKey: ${JSON.stringify(publicKey)},${titleLine}
  });
</script>`;
}
