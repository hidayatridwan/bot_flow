/**
 * Decoding the SSE stream that `POST /ask/stream` produces.
 *
 * The contract is owned by `crates/api/src/handlers.rs` (`ask_stream`) and by axum's `Event`
 * serialiser underneath it. `widget/widget.js` has a second, untested implementation of the same
 * contract; it cannot be shared, because the widget has no build step and so no module boundary to
 * import through. This is the tested one — when the two disagree, the API's bytes decide.
 *
 * Three properties of those bytes drive this design, and each is a trap:
 *
 * 1. **A token containing a newline arrives as several `data:` lines.** axum re-prefixes every `\n`
 *    and `\r` inside a field value with a fresh `data: ` (see its `EventDataWriter::write_buf`), so
 *    an answer with a paragraph break is split across lines on the wire. Rejoining them with `\n` is
 *    what puts the paragraph back; skip it and prose silently loses its line breaks.
 *
 * 2. **The keep-alive is a comment-only frame.** `KeepAlive::default()` is the literal bytes
 *    `:\n\n`, emitted every ~15s on an idle stream. It must decode to *nothing*, not to an empty
 *    frame — a consumer switching on `event` would otherwise see a stray `message` every 15s.
 *
 * 3. **`event: done` carries no `data:` line at all**, because `Event::data("")` writes nothing (its
 *    `write_buf` returns early on an empty slice). This is why the decoder deliberately **departs
 *    from the WHATWG spec**, whose dispatch step reads: *"If the data buffer is an empty string, set
 *    the data buffer and the event type buffer to the empty string and return"* — i.e. a
 *    data-less event is never dispatched. Honouring that would drop `done`, which is the stream's
 *    terminal sentinel, and every completed answer would look truncated. So we dispatch any frame
 *    that carried at least one field, and only comment-only frames vanish.
 *
 *    The corollary is worth knowing before anyone points a browser at this endpoint directly: a real
 *    `EventSource` implements the spec, so it would **never fire `done`** — silently. Our consumers
 *    read the body with `fetch` and this decoder, so they are unaffected.
 *
 * Line splitting follows the spec proper: CRLF, CR and LF are all terminators.
 */

export interface SseFrame {
	/** The `event:` field, or `message` when the frame omitted one — per the spec's default. */
	readonly event: string;
	/** Every `data:` line of the frame, rejoined with `\n`. `''` when the frame carried none. */
	readonly data: string;
}

export interface SseDecoder {
	/** Feed one decoded chunk. Returns whatever frames it completed — often none. */
	push(chunk: string): SseFrame[];
}

const DEFAULT_EVENT = 'message';

export function createSseDecoder(): SseDecoder {
	let buffer = '';
	let event = '';
	let data: string[] = [];
	// Whether the frame in progress carried any field we recognise. This — not `data.length` — is
	// what decides dispatch, which is the whole of point 3 above.
	let fielded = false;

	function dispatch(): SseFrame | null {
		const frame = fielded ? { event: event || DEFAULT_EVENT, data: data.join('\n') } : null;
		event = '';
		data = [];
		fielded = false;
		return frame;
	}

	function consume(line: string): SseFrame | null {
		if (line === '') return dispatch();
		if (line.startsWith(':')) return null; // a comment; the keep-alive is nothing else

		const colon = line.indexOf(':');
		const field = colon === -1 ? line : line.slice(0, colon);
		let value = colon === -1 ? '' : line.slice(colon + 1);
		// Exactly one space, and only if present: `data:x` and `data: x` both mean `x`, but
		// `data:  x` means ` x`. axum always writes the space.
		if (value.startsWith(' ')) value = value.slice(1);

		if (field === 'event') {
			event = value;
			fielded = true;
		} else if (field === 'data') {
			data.push(value);
			fielded = true;
		}
		// `id` and `retry` are spec fields this API never sends, and any other name is ignored per
		// spec. None of them mark the frame as fielded: a frame of nothing but unknown fields has no
		// content to hand anyone.
		return null;
	}

	return {
		push(chunk: string): SseFrame[] {
			buffer += chunk;
			const frames: SseFrame[] = [];
			let start = 0;
			let i = 0;

			while (i < buffer.length) {
				const c = buffer[i];
				if (c === '\n') {
					const frame = consume(buffer.slice(start, i));
					if (frame) frames.push(frame);
					i += 1;
					start = i;
				} else if (c === '\r') {
					// A trailing `\r` is ambiguous — it may be the first half of a `\r\n` whose second
					// half is in the next chunk. Leave it buffered rather than guess, or the line
					// break doubles.
					if (i === buffer.length - 1) break;
					const frame = consume(buffer.slice(start, i));
					if (frame) frames.push(frame);
					i += buffer[i + 1] === '\n' ? 2 : 1;
					start = i;
				} else {
					i += 1;
				}
			}

			// Whatever follows the last terminator is an incomplete line. Hold it: the spec discards a
			// trailing block that never gets its blank line, and so must we — a half-arrived frame is
			// not a frame.
			buffer = buffer.slice(start);
			return frames;
		}
	};
}
