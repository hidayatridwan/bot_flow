import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ask, type AskHandlers, type Source } from './ask';
import { CUT_SHORT, NO_WORDS, SESSION_EXPIRED } from './error-map';
import { GENERIC, RATE_LIMITED, UNREACHABLE } from '$lib/utils/api-copy';

/** A streaming Response, delivered in whatever chunks the test asks for. */
function stream(chunks: string[], init: ResponseInit = {}) {
	const body = new ReadableStream<Uint8Array>({
		start(c) {
			for (const chunk of chunks) c.enqueue(new TextEncoder().encode(chunk));
			c.close();
		}
	});
	return new Response(body, {
		status: 200,
		headers: { 'content-type': 'text/event-stream' },
		...init
	});
}

const jsonError = (status: number, error: unknown) =>
	new Response(JSON.stringify({ error }), {
		status,
		headers: { 'content-type': 'application/json' }
	});

/** What the API sends when retrieval found passages. */
const ANSWERED = [
	'event: conversation\ndata: e4490fbb-e9ca-4c8b-845c-fb39f31ae699\n\n',
	'event: sources\ndata: [{"index":1,"score":0.54,"document_id":"d1","text":"Refunds within 30 days."}]\n\n',
	'event: token\ndata: Refunds\n\n',
	'event: token\ndata:  are accepted.\n\n',
	'event: done\n\n'
];

/** What it sends when nothing cleared the floor — an empty array and the canned line. */
const REFUSED = [
	'event: conversation\ndata: e4490fbb-e9ca-4c8b-845c-fb39f31ae699\n\n',
	'event: sources\ndata: []\n\n',
	"event: token\ndata: Sorry, I couldn't find any relevant information.\n\n",
	'event: done\n\n'
];

let handlers: AskHandlers & {
	tokens: string[];
	conversations: string[];
	sourceSets: Source[][];
};

beforeEach(() => {
	const tokens: string[] = [];
	const conversations: string[] = [];
	const sourceSets: Source[][] = [];
	handlers = {
		tokens,
		conversations,
		sourceSets,
		onToken: (t) => tokens.push(t),
		onConversation: (id) => conversations.push(id),
		onSources: (s) => sourceSets.push(s)
	};
});

describe('ask', () => {
	describe('the request it makes', () => {
		it('calls our own origin with a relative path and no credential', async () => {
			// The boundary this file exists to defend. An absolute API URL would be the browser talking
			// to Rust directly — which it cannot authenticate to, since the session is httpOnly and
			// unreadable from here. Same assertion, same reason, as upload.test.ts.
			const fetch = vi.fn().mockResolvedValue(stream(ANSWERED));
			await ask({ query: 'hi' }, handlers, { fetch });

			expect(fetch.mock.calls[0][0]).toBe('/playground/ask');
			expect(fetch.mock.calls[0][0]).not.toMatch(/^https?:\/\//);
			const init = fetch.mock.calls[0][1];
			expect(init.headers).not.toHaveProperty('authorization');
			expect(init.headers).not.toHaveProperty('cookie');
		});

		it("sends '' for conversationId on the first turn", async () => {
			// The API's `empty_string_as_none` treats absent and '' alike, so turn one needs no special
			// case anywhere. Pinned because the alternative — omitting the key — also works, and a
			// refactor that "tidies" one into the other must not change behaviour silently.
			const fetch = vi.fn().mockResolvedValue(stream(ANSWERED));
			await ask({ query: 'hi' }, handlers, { fetch });

			expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual({ query: 'hi', conversationId: '' });
		});

		it('passes a conversation id through when it has one', async () => {
			const fetch = vi.fn().mockResolvedValue(stream(ANSWERED));
			await ask({ query: 'and then?', conversationId: 'abc-123' }, handlers, { fetch });

			expect(JSON.parse(fetch.mock.calls[0][1].body).conversationId).toBe('abc-123');
		});
	});

	describe('a successful answer', () => {
		it('drives every frame into its handler', async () => {
			const fetch = vi.fn().mockResolvedValue(stream(ANSWERED));
			const out = await ask({ query: 'hi' }, handlers, { fetch });

			expect(out).toEqual({ ok: true, refused: false });
			expect(handlers.conversations).toEqual(['e4490fbb-e9ca-4c8b-845c-fb39f31ae699']);
			expect(handlers.tokens.join('')).toBe('Refunds are accepted.');
			expect(handlers.sourceSets).toHaveLength(1);
		});

		it('maps sources to the domain shape, keeping index as sent', async () => {
			// `index` is 1-based from the API and must never be renumbered — the model is forbidden from
			// writing markers, so it is the only path from prose back to a passage (invariant 5).
			const fetch = vi.fn().mockResolvedValue(stream(ANSWERED));
			await ask({ query: 'hi' }, handlers, { fetch });

			expect(handlers.sourceSets[0]).toEqual([
				{ index: 1, score: 0.54, documentId: 'd1', text: 'Refunds within 30 days.' }
			]);
		});

		it('does not renumber a non-sequential index', async () => {
			// The API always sends 1..n in order today, so nothing else would catch a `#{i+1}` refactor.
			const fetch = vi
				.fn()
				.mockResolvedValue(
					stream([
						'event: sources\ndata: [{"index":7,"score":0.4,"document_id":"d","text":"t"}]\n\n',
						'event: token\ndata: x\n\n',
						'event: done\n\n'
					])
				);
			await ask({ query: 'hi' }, handlers, { fetch });

			expect(handlers.sourceSets[0][0].index).toBe(7);
		});

		it('reassembles tokens torn across chunk boundaries', async () => {
			// TCP does not respect frame edges. The decoder handles this; the assertion is that `ask`
			// does not undo it by decoding each chunk in isolation.
			const whole = ANSWERED.join('');
			const fetch = vi
				.fn()
				.mockResolvedValue(stream([whole.slice(0, 61), whole.slice(61, 140), whole.slice(140)]));
			const out = await ask({ query: 'hi' }, handlers, { fetch });

			expect(out).toEqual({ ok: true, refused: false });
			expect(handlers.tokens.join('')).toBe('Refunds are accepted.');
		});

		it('keeps a multi-byte character split across chunks intact', async () => {
			// Decoding without `stream: true` replaces the halves with U+FFFD, and the documents are
			// multilingual — this is the TS twin of the "index over chars" trap in Rust.
			const bytes = new TextEncoder().encode('event: token\ndata: 日本語\n\nevent: done\n\n');
			const body = new ReadableStream<Uint8Array>({
				start(c) {
					c.enqueue(bytes.slice(0, 21)); // mid-character
					c.enqueue(bytes.slice(21));
					c.close();
				}
			});
			const fetch = vi
				.fn()
				.mockResolvedValue(
					new Response(body, { headers: { 'content-type': 'text/event-stream' } })
				);
			await ask({ query: 'hi' }, handlers, { fetch });

			expect(handlers.tokens.join('')).toBe('日本語');
		});
	});

	describe('a refusal is an answer, not an error (invariant 4)', () => {
		it('reports ok with refused, so the caller can render it as a reply', async () => {
			const fetch = vi.fn().mockResolvedValue(stream(REFUSED));
			const out = await ask({ query: 'who won in 1994?' }, handlers, { fetch });

			expect(out).toEqual({ ok: true, refused: true });
			// The canned line still reaches the transcript — it is what the user is told.
			expect(handlers.tokens.join('')).toBe("Sorry, I couldn't find any relevant information.");
		});

		it('decides by structure, never by matching the canned text', async () => {
			// NO_ANSWER is a Rust constant and will drift the first time someone rewords it. A client
			// matching on the text would then start calling refusals real answers — silently, and in the
			// direction that matters: the tenant would believe their bot answered.
			const reworded = [
				'event: sources\ndata: []\n\n',
				'event: token\ndata: I do not have that information in your documents.\n\n',
				'event: done\n\n'
			];
			const fetch = vi.fn().mockResolvedValue(stream(reworded));
			expect(await ask({ query: 'x' }, handlers, { fetch })).toEqual({ ok: true, refused: true });
		});

		it('does not call a real answer refused just because it is short', async () => {
			const fetch = vi.fn().mockResolvedValue(stream(ANSWERED));
			expect(await ask({ query: 'x' }, handlers, { fetch })).toEqual({ ok: true, refused: false });
		});
	});

	describe('a stream that found passages and produced no words', () => {
		// Observed in the wild, not invented. Every reasoning model bills its thinking against the same
		// max_tokens budget as its prose, so a question that thinks hard enough spends the lot and emits
		// no `content` at all. Reproduced against the configured gateway: at a squeezed budget,
		// finish_reason comes back "length" with zero content deltas. Nothing failed, so the API quite
		// correctly yields `done` — only the client is placed to notice the silence.
		const SILENT = [
			'event: conversation\ndata: e4490fbb-e9ca-4c8b-845c-fb39f31ae699\n\n',
			'event: sources\ndata: [{"index":1,"score":0.59,"document_id":"d1","text":"Refunds within 30 days."}]\n\n',
			'event: done\n\n'
		];

		it('reports a failure rather than rendering silence', async () => {
			const fetch = vi.fn().mockResolvedValue(stream(SILENT));
			const out = await ask({ query: 'dimana dia bekerja terakhir?' }, handlers, { fetch });

			expect(out).toEqual({ ok: false, message: NO_WORDS });
			// The citations still arrived — the caller may render them beside the failure.
			expect(handlers.sourceSets[0]).toHaveLength(1);
		});

		it('does not mistake a refusal for silence', async () => {
			// The mirror case, and the one that must not regress: a refusal has no sources but DOES carry
			// its canned sentence, so it stays a successful answer (invariant 4).
			const fetch = vi.fn().mockResolvedValue(stream(REFUSED));
			expect(await ask({ query: 'x' }, handlers, { fetch })).toEqual({ ok: true, refused: true });
		});

		it('does not fire when a single token arrived', async () => {
			const fetch = vi
				.fn()
				.mockResolvedValue(
					stream([
						'event: sources\ndata: [{"index":1,"score":0.5,"document_id":"d","text":"t"}]\n\n',
						'event: token\ndata: Yes.\n\n',
						'event: done\n\n'
					])
				);
			expect(await ask({ query: 'x' }, handlers, { fetch })).toEqual({ ok: true, refused: false });
		});

		it('counts tokens rather than judging the prose', async () => {
			// A whitespace-only answer is still the model speaking. Inferring emptiness from the joined
			// text would silently reclassify it, and it is not this file's place to grade output.
			const fetch = vi
				.fn()
				.mockResolvedValue(
					stream([
						'event: sources\ndata: [{"index":1,"score":0.5,"document_id":"d","text":"t"}]\n\n',
						'event: token\ndata:  \n\n',
						'event: done\n\n'
					])
				);
			expect(await ask({ query: 'x' }, handlers, { fetch })).toEqual({ ok: true, refused: false });
		});
	});

	describe('the error frame (invariant 16)', () => {
		it('never surfaces the frame text, and logs it instead', async () => {
			// The API already replaces this with a fixed string. Held again here because a client must
			// not render detail it did not author — the API is the enforcement point, this is the belt.
			const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
			const fetch = vi
				.fn()
				.mockResolvedValue(
					stream([
						'event: sources\ndata: [{"index":1,"score":0.5,"document_id":"d","text":"t"}]\n\n',
						'event: token\ndata: The refund\n\n',
						'event: error\ndata: LLM replied 401: {"message":"Incorrect API key sk-abc123"}\n\n'
					])
				);
			const out = await ask({ query: 'hi' }, handlers, { fetch });

			expect(out).toEqual({ ok: false, message: GENERIC });
			if (out.ok) return;
			expect(out.message).not.toMatch(/sk-abc123|LLM replied|401/);
			expect(spy).toHaveBeenCalled();
			spy.mockRestore();
		});

		it('keeps the tokens that already arrived', async () => {
			// They are on screen and they are true. The failure is about the rest.
			const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
			const fetch = vi
				.fn()
				.mockResolvedValue(
					stream(['event: token\ndata: The refund\n\n', 'event: error\ndata: boom\n\n'])
				);
			await ask({ query: 'hi' }, handlers, { fetch });

			expect(handlers.tokens.join('')).toBe('The refund');
			spy.mockRestore();
		});
	});

	describe('terminal states', () => {
		it('treats a stream that ends without `done` as cut short', async () => {
			const fetch = vi
				.fn()
				.mockResolvedValue(
					stream([
						'event: sources\ndata: [{"index":1,"score":0.5,"document_id":"d","text":"t"}]\n\n',
						'event: token\ndata: partial\n\n'
					])
				);
			const out = await ask({ query: 'hi' }, handlers, { fetch });

			expect(out).toEqual({ ok: false, message: CUT_SHORT });
			expect(handlers.tokens.join('')).toBe('partial');
		});

		it('stops at `done` and ignores anything after it', async () => {
			const fetch = vi
				.fn()
				.mockResolvedValue(stream([...ANSWERED, 'event: token\ndata: stray\n\n']));
			const out = await ask({ query: 'hi' }, handlers, { fetch });

			expect(out).toEqual({ ok: true, refused: false });
			expect(handlers.tokens.join('')).toBe('Refunds are accepted.');
		});

		it('survives the keep-alive on an idle stream', async () => {
			const fetch = vi.fn().mockResolvedValue(stream([':\n\n', ...ANSWERED, ':\n\n']));
			expect(await ask({ query: 'hi' }, handlers, { fetch })).toEqual({ ok: true, refused: false });
		});
	});

	describe('failures before the stream opens', () => {
		it('maps a 429 to the rate-limit copy', async () => {
			const fetch = vi.fn().mockResolvedValue(
				jsonError(429, {
					status: 429,
					message: 'rate limit exceeded',
					kind: 'client',
					path: '/x'
				})
			);
			expect(await ask({ query: 'hi' }, handlers, { fetch })).toEqual({
				ok: false,
				message: RATE_LIMITED
			});
		});

		it('maps a 401 without needing the envelope', async () => {
			// The route's own guards use SvelteKit's `error()`, which serialises to `{message}` — not our
			// `{error: ApiError}` envelope. 401 is pulled out ahead of the parse for exactly that reason.
			const fetch = vi
				.fn()
				.mockResolvedValue(
					new Response(JSON.stringify({ message: 'not signed in' }), { status: 401 })
				);
			expect(await ask({ query: 'hi' }, handlers, { fetch })).toEqual({
				ok: false,
				message: SESSION_EXPIRED
			});
		});

		it('never leaks an internal message for a 5xx', async () => {
			const fetch = vi.fn().mockResolvedValue(
				jsonError(500, {
					status: 500,
					message: 'thread panicked at handlers.rs:512',
					kind: 'server',
					path: '/x'
				})
			);
			const out = await ask({ query: 'hi' }, handlers, { fetch });
			expect(out).toEqual({ ok: false, message: GENERIC });
		});

		it('reports an unreachable BFF as a connection problem', async () => {
			const fetch = vi.fn().mockRejectedValue(new TypeError('fetch failed'));
			expect(await ask({ query: 'hi' }, handlers, { fetch })).toEqual({
				ok: false,
				message: UNREACHABLE
			});
		});

		it('does not choke on an error response that is not json', async () => {
			const fetch = vi.fn().mockResolvedValue(new Response('502 Bad Gateway', { status: 502 }));
			expect(await ask({ query: 'hi' }, handlers, { fetch })).toEqual({
				ok: false,
				message: GENERIC
			});
		});
	});
});
