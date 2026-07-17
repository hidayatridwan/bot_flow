import { error, json } from '@sveltejs/kit';
import type { RequestHandler } from './$types';
import { apiStream } from '$lib/server/api';
import { askStream } from '$lib/server/api/chat';
import { isUuid } from '$lib/utils/uuid';

/**
 * Ask one question and pipe the answer's SSE stream back to the browser.
 *
 * **Why the bytes come through Node at all**, when the upload path deliberately avoids exactly that.
 * The two are opposites on purpose. An upload hands the browser a *capability* — a presigned URL good
 * for one key, one method, one TTL — so the bytes can skip this process entirely (invariant 24).
 * There is no equivalent for asking a question: the only credential that opens `/ask/stream` for the
 * dashboard is the session, and the session must never leave the server (invariant 20). A `pk_` is
 * not an option either — it would have to be minted per page load and allow-listed for this origin,
 * which is invariant 15's containment turned inside out. So the session stays in `locals`, goes
 * upstream as a Bearer header, and what returns to the browser is bytes. That is the more
 * conservative of the two designs, not the less.
 *
 * This is the second BFF route a browser fetches directly, and it mirrors `documents/upload-url`
 * clause for clause below — same origin check, same content-type check, same reason for reading
 * `locals.session` by hand. If you change one, read the other.
 */

export const POST: RequestHandler = async (event) => {
	const { request, url, locals } = event;

	// `bf_session` is SameSite=Lax, so a cross-site fetch POST does not carry it — that alone closes
	// CSRF here. SvelteKit's own `csrf.checkOrigin` does not: it only covers form-encodable content
	// types and never sees a JSON POST. Belt to Lax's braces, exactly as in upload-url.
	if (request.headers.get('origin') !== url.origin) {
		error(403, 'forbidden');
	}
	if (!request.headers.get('content-type')?.includes('application/json')) {
		error(415, 'expected application/json');
	}

	// NOT `requireUser`/`requireSession`: those throw a 303 to /login, and `fetch` follows redirects —
	// the caller would get the login page's HTML with a 200 and choke reading it as a stream. A
	// fetched endpoint answers with a status the client can branch on.
	if (!locals.session) {
		error(401, 'not signed in');
	}

	let body: unknown;
	try {
		body = await request.json();
	} catch {
		error(400, 'invalid json');
	}

	const { query, conversationId } = (body ?? {}) as { query?: unknown; conversationId?: unknown };

	// The API does *not* reject an empty query — it would embed the empty string and bill us for it.
	// This is the only place that guard exists, so it is stricter than the wire contract rather than a
	// mirror of it.
	const trimmed = typeof query === 'string' ? query.trim() : '';
	if (!trimmed) {
		error(400, 'expected a query');
	}

	// Absent and `''` both mean "start a new conversation" — the API's `empty_string_as_none` treats
	// them alike, so turn one needs no special case. Anything else must be a real UUID: the API would
	// 422 it anyway, but a BFF that relays junk upstream is just a slower way to get the same answer.
	const hasConversation =
		conversationId !== undefined && conversationId !== null && conversationId !== '';
	if (hasConversation && !isUuid(conversationId)) {
		error(400, 'invalid conversationId');
	}

	const res = await askStream(apiStream(event, locals.session.token), {
		query: trimmed,
		conversation_id: hasConversation ? (conversationId as string) : ''
	});

	if (!res.ok) {
		// A failure here is never mid-stream: auth, rate limiting, the query rewrite and retrieval all
		// run before the API constructs its SSE response. So the client sees either this JSON envelope
		// or a clean stream, never a stream that begins with an apology. `kind` is what lets it tell
		// "you did something wrong" from "we are broken" without reading `message` (invariant 16).
		return json({ error: res.error }, { status: res.error.status || 503 });
	}

	return new Response(res.body, {
		status: res.status,
		headers: {
			'content-type': 'text/event-stream',
			// `no-transform` is the load-bearing half. Without it a proxy may gzip the stream, and a
			// compressor with a buffer coalesces the tokens into one blob at the end — the response
			// stays byte-identical, every test still passes, and the feature silently stops being a
			// stream.
			'cache-control': 'no-cache, no-transform',
			// The same failure, from nginx specifically: it buffers `proxy_pass` by default. This is a
			// no-op elsewhere and the difference between working and not in front of nginx.
			'x-accel-buffering': 'no'
		}
	});
};
