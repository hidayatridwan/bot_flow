import type { StreamClient } from './stream';

/**
 * The wire shape for the ask routes. snake_case stops here, exactly as in `documents.ts`.
 *
 * Only the streaming route is wired. `POST /ask` returns the same answer as one JSON blob and has no
 * caller: the playground exists to show a tenant what their end users see, and what they see is
 * tokens arriving. Adding the JSON twin here would be a second path to keep working with no one on it.
 */

export interface AskStreamBody {
	query: string;
	/**
	 * Omit — or send `''` — to start a new conversation.
	 *
	 * The API's `empty_string_as_none` treats absent, `null`, `''` and whitespace alike, so a form
	 * field that starts empty needs no special casing on the first turn. A non-empty value that is not
	 * a UUID is a 422, and a well-formed UUID belonging to another tenant is a 404 — never a 403,
	 * which would make this an oracle for which conversations exist (invariant 8).
	 */
	conversation_id?: string;
	limit?: number;
}

/**
 * Returns the raw SSE body. The frame contract — and the reason `done` needs a bespoke decoder — is
 * in `features/chat/sse.ts`.
 */
export const askStream = (client: StreamClient, body: AskStreamBody) =>
	client.post('/ask/stream', body);
