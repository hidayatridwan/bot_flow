import { describe, expect, it } from 'vitest';
import { createSseDecoder } from './sse';

/** Feed a whole string as one chunk and collect the frames. */
const decode = (raw: string) => createSseDecoder().push(raw);

/** Feed a string one character at a time — the worst case any network can hand us. */
function decodeByChar(raw: string) {
	const decoder = createSseDecoder();
	const frames = [];
	for (const c of raw) frames.push(...decoder.push(c));
	return frames;
}

describe('createSseDecoder', () => {
	// Captured verbatim from `curl -sN POST /ask/stream` against a live API. If the API's framing
	// ever moves, this is the test that should fail first.
	const REAL_STREAM =
		'event: conversation\n' +
		'data: e4490fbb-e9ca-4c8b-845c-fb39f31ae699\n' +
		'\n' +
		'event: sources\n' +
		'data: [{"document_id":"","index":1,"score":0.63,"text":"Our refund policy."}]\n' +
		'\n' +
		'event: token\n' +
		'data: You\n' +
		'\n' +
		'event: done\n' +
		'\n';

	it('decodes a real stream, in order', () => {
		expect(decode(REAL_STREAM).map((f) => f.event)).toEqual([
			'conversation',
			'sources',
			'token',
			'done'
		]);
	});

	it('dispatches `done`, which carries no data field at all', () => {
		// The reason this decoder departs from the spec. `Event::data("")` writes nothing, so `done`
		// is a bare `event:` line. The spec's dispatch step drops a data-less event — honouring it
		// would lose the terminal sentinel and make every completed answer look truncated.
		const frames = decode('event: done\n\n');
		expect(frames).toEqual([{ event: 'done', data: '' }]);
	});

	it('yields nothing for the keep-alive', () => {
		// `KeepAlive::default()` is the literal bytes `:\n\n`, every ~15s on an idle stream. An empty
		// frame here would reach a consumer's `switch` as a stray `message`.
		expect(decode(':\n\n')).toEqual([]);
		expect(decode(':\n\n:\n\nevent: done\n\n')).toEqual([{ event: 'done', data: '' }]);
	});

	it('rejoins a multi-line token, restoring the newline axum split on', () => {
		// axum re-prefixes every `\n` inside a value with a fresh `data: `. Joining with `\n` is what
		// puts the paragraph break back; without it the prose silently runs together.
		expect(decode('event: token\ndata: first\ndata: second\n\n')).toEqual([
			{ event: 'token', data: 'first\nsecond' }
		]);
	});

	it('survives a frame split across chunk boundaries at every position', () => {
		// The likeliest real bug: TCP does not respect frame boundaries. Splitting at the `\n\n` is
		// only the most obvious case, so try them all.
		const whole = decode(REAL_STREAM);
		for (let at = 1; at < REAL_STREAM.length; at++) {
			const decoder = createSseDecoder();
			const frames = [
				...decoder.push(REAL_STREAM.slice(0, at)),
				...decoder.push(REAL_STREAM.slice(at))
			];
			expect(frames, `split at ${at}`).toEqual(whole);
		}
	});

	it('survives being fed one character at a time', () => {
		expect(decodeByChar(REAL_STREAM)).toEqual(decode(REAL_STREAM));
	});

	it('keeps the sources JSON byte-for-byte, so it still parses', () => {
		const json = '[{"document_id":"","index":1,"score":0.63,"text":"a: b, c"}]';
		const [frame] = decode(`event: sources\ndata: ${json}\n\n`);
		// The colon inside the payload must not be mistaken for a field separator — only the first
		// colon on the line is one.
		expect(frame.data).toBe(json);
		expect(JSON.parse(frame.data)[0].text).toBe('a: b, c');
	});

	it('treats CRLF and CR as terminators, like the spec', () => {
		expect(decode('event: token\r\ndata: hi\r\n\r\n')).toEqual([{ event: 'token', data: 'hi' }]);
		// Bare CR, resolved because a non-`\n` byte follows the final terminator.
		expect(decode('event: token\rdata: hi\r\rdata: next\n')).toEqual([
			{ event: 'token', data: 'hi' }
		]);
	});

	it('never splits a CRLF into two terminators', () => {
		// The reason a trailing `\r` cannot simply be guessed at. Treating CR and LF as separate
		// terminators here would dispatch `{data: 'a'}` at the CRLF and strand `b` — one frame
		// becomes two, and the second half of the answer is lost.
		expect(decode('data: a\r\ndata: b\n\n')).toEqual([{ event: 'message', data: 'a\nb' }]);
	});

	it('holds a trailing lone CR rather than guessing what follows it', () => {
		// Genuinely ambiguous mid-stream: the next chunk decides whether this was a bare CR or half a
		// CRLF. Our API never ends a stream this way — axum's `field` always terminates with `\n`, so
		// a real stream's last bytes are `event: done\n\n`.
		const decoder = createSseDecoder();
		expect(decoder.push('data: hi\r')).toEqual([]);
		expect(decoder.push('\n\n')).toEqual([{ event: 'message', data: 'hi' }]);
	});

	it('does not double a line break when CRLF is torn across chunks', () => {
		// A trailing `\r` is ambiguous until the next byte arrives. Guessing it was a bare CR would
		// end the line early and dispatch a frame that has not finished.
		const decoder = createSseDecoder();
		expect(decoder.push('event: token\r')).toEqual([]);
		expect(decoder.push('\ndata: hi\r\n\r\n')).toEqual([{ event: 'token', data: 'hi' }]);
	});

	it('holds back a frame that has not got its blank line yet', () => {
		const decoder = createSseDecoder();
		expect(decoder.push('event: token\ndata: partial\n')).toEqual([]);
		expect(decoder.push('\n')).toEqual([{ event: 'token', data: 'partial' }]);
	});

	it('defaults a data-only frame to `message`', () => {
		expect(decode('data: bare\n\n')).toEqual([{ event: 'message', data: 'bare' }]);
	});

	it('strips exactly one leading space from a value', () => {
		// `data:x` and `data: x` are both `x`; a second space is content. An LLM token legitimately
		// starts with a space (' cannot'), and eating it would run words together.
		expect(decode('data:x\n\n')[0].data).toBe('x');
		expect(decode('data: x\n\n')[0].data).toBe('x');
		expect(decode('data:  x\n\n')[0].data).toBe(' x');
	});

	it('preserves a token that is only a space', () => {
		// axum writes `data: ` + ' ' = `data:  `, and the decoder must give the space back — this is
		// how word boundaries survive a token stream.
		expect(decode('event: token\ndata:  \n\n')).toEqual([{ event: 'token', data: ' ' }]);
	});

	it('ignores spec fields this API never sends, without inventing a frame', () => {
		expect(decode('id: 1\nretry: 500\n\n')).toEqual([]);
		expect(decode('id: 1\nevent: token\ndata: hi\n\n')).toEqual([{ event: 'token', data: 'hi' }]);
	});

	it('does not leak state between frames', () => {
		// `event` is per-frame. A `token` following a `sources` must not inherit its type, and a
		// dispatched frame's data must not reappear in the next one.
		expect(decode('event: sources\ndata: []\n\ndata: after\n\n')).toEqual([
			{ event: 'sources', data: '[]' },
			{ event: 'message', data: 'after' }
		]);
	});

	it('is unbothered by blank lines between frames', () => {
		expect(decode('\n\n\nevent: done\n\n')).toEqual([{ event: 'done', data: '' }]);
	});
});
