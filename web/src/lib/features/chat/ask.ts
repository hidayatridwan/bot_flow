import type { ApiError } from '$lib/types/api';
import { GENERIC, UNREACHABLE } from '$lib/utils/api-copy';
import { CUT_SHORT, NO_WORDS, SESSION_EXPIRED, mapAskError } from './error-map';
import { createSseDecoder } from './sse';

/**
 * Ask one question and drive the answer's frames into the caller's handlers.
 *
 * `fetch` is injected rather than imported, exactly as in `upload.ts`, so the boundary this file
 * defends is assertable in a unit test. What it defends:
 *
 * - **It calls our own origin, and carries no credential.** `ASK_PATH` is relative on purpose. The
 *   session lives in an `httpOnly` cookie this code cannot read even if it wanted to, and the BFF is
 *   what turns it into a Bearer header (invariant 20). An absolute API URL here would be a browser
 *   talking to Rust directly — which it cannot authenticate to, and must not.
 * - **A refusal is an answer** (invariant 4), not an error. See `refused` below.
 * - **The `error` frame's text is discarded** (invariant 16), never rendered.
 */

export interface Source {
	/** 1-based, and never renumbered — the only thing tying prose back to a passage (invariant 5). */
	index: number;
	score: number;
	documentId: string;
	text: string;
}

export type AskOutcome =
	| {
			readonly ok: true;
			/**
			 * Nothing cleared the relevance floor, so the API answered with its canned line and never
			 * called the model. Still `ok: true` — this is the system keeping its promise, and the
			 * caller should render it as an ordinary reply.
			 */
			readonly refused: boolean;
	  }
	| { readonly ok: false; readonly message: string };

export interface AskHandlers {
	onConversation(id: string): void;
	onSources(sources: Source[]): void;
	onToken(text: string): void;
}

export interface AskDeps {
	fetch: typeof globalThis.fetch;
}

/** Our own BFF endpoint. Relative on purpose — it must never be an absolute API URL. */
const ASK_PATH = '/playground/ask';

interface SourceDto {
	index: number;
	score: number;
	document_id: string;
	text: string;
}

/** snake_case stops here, as it does in `server/api/`. */
const toSource = (dto: SourceDto): Source => ({
	index: dto.index,
	score: dto.score,
	documentId: dto.document_id,
	text: dto.text
});

/**
 * The route answers in two different error shapes, and both are reachable.
 *
 * Its own guards use SvelteKit's `error()`, which serialises to `{message}`. A failure relayed from
 * the API is `json({error})`, our `ApiError` envelope. Only the second can be mapped to useful copy;
 * the first means *we* sent a malformed request, which is our bug and not something to explain to a
 * user. 401 is pulled out ahead of the parse because it is the one status a user can act on.
 */
async function messageForFailure(res: Response): Promise<string> {
	if (res.status === 401) return SESSION_EXPIRED;

	let payload: unknown;
	try {
		payload = await res.json();
	} catch {
		return GENERIC;
	}
	const error = (payload as { error?: ApiError } | null)?.error;
	return error ? mapAskError(error) : GENERIC;
}

export async function ask(
	input: { query: string; conversationId?: string },
	handlers: AskHandlers,
	deps: AskDeps
): Promise<AskOutcome> {
	let res: Response;
	try {
		res = await deps.fetch(ASK_PATH, {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ query: input.query, conversationId: input.conversationId ?? '' })
		});
	} catch {
		return { ok: false, message: UNREACHABLE };
	}

	// A non-2xx never streams. Auth, rate limiting, the query rewrite and retrieval all run before the
	// API builds its SSE response, so a failure is a whole short body — never a stream that opens and
	// then apologises.
	if (!res.ok) return { ok: false, message: await messageForFailure(res) };
	if (!res.body) return { ok: false, message: GENERIC };

	const decoder = createSseDecoder();
	const reader = res.body.getReader();
	const utf8 = new TextDecoder();

	let sources: Source[] = [];
	let done = false;
	let failure: string | null = null;
	// Counted rather than inferred from the accumulated text: a stream of whitespace-only tokens is
	// still the model speaking, and it is not this file's job to judge the prose.
	let tokens = 0;

	reading: for (;;) {
		let chunk: ReadableStreamReadResult<Uint8Array>;
		try {
			chunk = await reader.read();
		} catch {
			break; // socket died; `done` stays false and the caller keeps whatever arrived
		}
		if (chunk.done) break;

		// `stream: true` matters: a multi-byte character can straddle a chunk boundary, and decoding
		// without it replaces the halves with U+FFFD. The documents are multilingual.
		for (const frame of decoder.push(utf8.decode(chunk.value, { stream: true }))) {
			switch (frame.event) {
				case 'conversation':
					handlers.onConversation(frame.data);
					break;
				case 'sources':
					try {
						sources = (JSON.parse(frame.data) as SourceDto[]).map(toSource);
					} catch {
						sources = [];
					}
					handlers.onSources(sources);
					break;
				case 'token':
					tokens += 1;
					handlers.onToken(frame.data);
					break;
				case 'error':
					// Invariant 16, held a second time. The API already replaced this with a fixed
					// string, so `frame.data` should be safe — but the whole point of that invariant is
					// that a client never renders detail it did not author, and a future frame is not
					// something this file can vouch for.
					console.error('[ask] the api reported a stream failure:', frame.data);
					failure = GENERIC;
					break reading;
				case 'done':
					done = true;
					break reading;
			}
		}
	}

	// Ordering is guaranteed by the API today but enforced nowhere, so nothing above depends on it:
	// only `done` and `error` are terminal, and every other frame is handled wherever it lands.
	if (failure) return { ok: false, message: failure };
	if (!done) return { ok: false, message: CUT_SHORT };

	// Invariant 4, decided by the *structure* of the response rather than by its words. `NO_ANSWER`
	// is a Rust string constant that will drift the first time someone rewords it, and the moment it
	// does, a client matching on the text starts calling refusals real answers. `relevant.is_empty()`
	// is the single predicate driving both the empty array and the canned token, so this is exact by
	// construction and cannot rot.
	const refused = sources.length === 0;

	// Retrieval worked and the model still said nothing. Reported as a failure even though the stream
	// succeeded, because from the asker's side nothing happened: a silent bubble is indistinguishable
	// from a broken page, and they cannot know that retrying is the fix.
	//
	// A refusal always carries its canned sentence, so this can only fire when passages were found.
	// The cause is upstream — a reasoning model bills its thinking against the same `max_tokens`
	// budget as its prose, so a hard enough question spends the lot and emits no `content`. Nothing
	// errored, so the API correctly yields `done`; only the client is placed to notice the silence.
	if (!refused && tokens === 0) return { ok: false, message: NO_WORDS };

	return { ok: true, refused };
}
