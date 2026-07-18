# Feature: The chat playground (phase 5)

> Status: **implemented.** `/ask` and `/ask/stream` widened to `Actor` and deliberately ungated
> (invariant 27, new); the SSE `error` frame sealed; a `chat` slice and `/playground` route in `web/`.
> CLAUDE.md gained invariant 27, a corrected invariant 20, amendments to 23/24, seven *Traps* rows and
> four *Known state* entries; README's citation claims were corrected and the Postman *Chat* group
> realigned. 185 web + 43 API unit tests; verified against a live stack by curl (the containment
> matrix) and in a real browser.

## Context — why

The Postman collection enumerates the API contract, and mapped against `web/` the **Chat** group was
the whole gap: `/ask`, `/ask/stream` and `/search` had no client function anywhere. Phase 4 had
already named this as next.

Two things followed from it, and only the first is obvious.

**A tenant could not verify their bot worked.** Register, upload a document, watch it reach `Ready`,
mint a `pk_` — and then the only way to ask a question was to embed the widget on a real site and try
it there. The product loop never closed in-product. Every prior phase built toward an answer nobody
could see without leaving.

**Citations rendered nowhere, and never had.** `widget/widget.js:147` says it plainly: *"The server
still emits a `sources` event — we ignore it rather than render it."* Invariant 5 forbids the model
from writing citation markers into its prose **because** citations come back as structured data
alongside the answer — that is the trade. The structured half had no consumer, so the system was
paying invariant 5's cost and collecting none of its benefit. The README claimed `[n]` markers that
the system prompt explicitly forbids; that claim was written against a system nobody had watched.

## The blocker

Both ask routes took `tenant: AuthTenant`, which resolves only `sk_`/`pk_` against `api_keys`. The
dashboard holds **only** a session: invariant 22 makes the one-time `sk_` unrecoverable and
`GET /auth/keys` returns hashes. A `sess_` sent to `AuthTenant` misses `api_keys` and 401s (invariant
17's disjoint tables, working exactly as designed).

So the dashboard could not ask a question at all. Not "was not wired up" — *could not*.

## The traps this phase existed to close

Two were live in production, and neither was found by reasoning about the code. Both were found by
looking at bytes.

### The SSE `error` frame streamed the gateway's raw body to browsers

`handlers.rs` yielded `Event::default().event("error").data(format!("{e:#}"))`, where `e` comes from
`llm.rs`'s `bail!("LLM replied {status}: {text}")` and `text` is **the gateway's error body verbatim**
— a body we do not author and cannot predict.

Proven with a canary `LLM_API_KEY`, not argued. A 401 from the configured gateway streamed a fragment
of the key, **the key's full SHA-256 hash**, and the gateway's internal table names — into a browser.
For a `pk_` widget, a *stranger's* browser.

This is invariant 16, and the reason it broke is structural: **a mid-stream frame never passes through
`AppError::into_response`.** Every other client-facing surface inherits invariant 16 from `?`. This one
had to re-implement the discipline by hand, and did not. It is now `STREAM_FAILED`, a fixed string,
with `tracing::error!` carrying the detail.

The API was the only possible enforcement point. The BFF is a dumb pipe by design — it hands
`upstream.body` over without parsing — so stripping the frame there would mean parsing and re-encoding
SSE server-side, forfeiting the whole point of the pipe. Discarding it in the renderer fixes nothing:
the bytes are still in the response, visible in the Network tab.

### The BFF sent the session cookie to the API on every call

Invariant 20 said the API stays cookie-free, and that CORS reasoning in invariant 18 rests on it. It
was **false**, and had always been.

`event.fetch` defaults to `credentials: 'same-origin'`, and Kit's notion of same-origin is a *hostname
suffix* match: it injects the browser's whole cookie jar whenever `` `.<apiHost>`.endsWith(`.<webHost>`) ``
— `localhost` → `localhost` in dev, `example.com` → `api.example.com` in production. It does this
*after* our header object is built, so no care in constructing headers prevents it and no assertion
over them can see it.

Found with an echo server. Never an auth hole — the API reads only `Authorization` — but `bf_session`
landed in the API's access logs, and invariant 18's "no cookie surface, therefore no CSRF surface"
rested on a fact that was not true. Both `client.ts` and `stream.ts` now pass `credentials: 'omit'`,
each pinned by a test, because this is invisible in review: **the code reads correctly either way.**

## Design

### Widening the ask routes to `Actor`, and calling no gate — invariant 27

The change is one word per handler. The commit's real content is the argument for why it is safe, and
that argument is the invariant.

A `pk_` is printed in public page source and is *expected to be stolen* — and it **already reached
these routes**, because "it can only ask questions" is precisely the containment that makes it safe to
print. So the ask routes are, by design, the ones the *weakest* credential in the system may reach. A
`sess_` costs an Argon2-verified password and expires; an `sk_` is stronger still. **Admitting the
stronger credentials to a route the weakest already reaches adds no exposure.**

That is why this is not the mirror of invariant 23. That gate refuses `pk_`; this one refuses nothing,
deliberately. `require_management()` here would 403 every deployed widget — the one client these routes
exist to serve — and it would be found by tenants, not by tests, because `Actor::from_request_parts`
needs a database and the unit tests cannot see it.

**The deferred spend question, answered by the limiter that already existed.** `rate_limit::check` keys
on `tenant_id`, not on the credential, and runs before the first LLM call. A session cannot draw a
token more than that tenant's own `pk_` already could.

Both routes were widened, not just the one with a caller. They share `AskRequest` and `prepare()` —
one endpoint, two transports. "A session works on `/ask` and 401s on `/ask/stream`" is indefensible and
a trap for the next reader. `auth.rs` needed **no code change**, and its gate tests passed
**untouched** — that is the evidence the management gate did not move.

### `chat/sse.ts` — the contract's only tested parser

Extracted from `widget.js`'s `_parseEvent` rather than shared with it: the widget has no build step, so
there is no module boundary to share through. The contract is owned by `handlers.rs`.

**It departs from the WHATWG spec on exactly one point, and says so.** `event: done` carries no `data:`
line — `Event::data("")` writes nothing — and the spec's dispatch step drops an event with an empty
data buffer. A browser `EventSource` would therefore *never fire `done`*, silently, and every completed
answer would look truncated. The decoder dispatches any frame with at least one field.

Two more that look like style and are not: multiple `data:` lines join with `'\n'`, because axum
re-prefixes every newline inside a value with a fresh `data:` — joining with `''` collapses a numbered
list into one line, and the answer is still *plausible*, which is why nothing would catch it. And
comment-only frames are dropped explicitly, because `KeepAlive::default()` emits `:\n\n` every ~15s and
the widget survives them by accident.

### `server/api/stream.ts` — a sibling to `client.ts`, not a flag on it

Two independent, fatal blocks. `parseResponse` opens with `await res.text()` — it consumes the body by
construction, and a stream is not a value. And `client.ts`'s `AbortSignal.timeout(10_000)` covers the
**body**, not just the headers, so it would kill every answer longer than ten seconds.

The ceiling that replaces it is `ASK_TIMEOUT_MS`, and 10s would have been wrong regardless:
`ask_stream` runs `prepare` (a full LLM rewrite) **and** `retrieve` (embedding + Qdrant) *before*
constructing the `Sse`, so headers do not arrive until two model calls complete.

The error path reuses `parseResponse` — a non-2xx is a short, complete body. Only the *success* path is
special.

### `ask.ts` — where invariant 4 is decided by structure

A refusal is detected by **`sources.length === 0`**, never by string-matching `NO_ANSWER`. That
constant lives in Rust and will drift the first time someone rewords it, and the moment it does, a
client matching on text starts calling refusals real answers. `relevant.is_empty()` drives both the
empty array and the canned token, so the structural check is exact by construction and cannot rot.

## Verification

**Unit**: 43 API tests (the `auth.rs` gate tests passing untouched is the load-bearing one); 185 web
tests across `sse.ts`, `stream.ts`, `ask.ts`, `sources.ts` and the `credentials: 'omit'` pins.

**The containment matrix, against a live stack** — no unit test can cover this, because
`Actor::from_request_parts` needs a database, and this phase touched a route live widgets already flow
through:

| | |
| --- | --- |
| `sess_` on `/ask` and `/ask/stream` | **200** ← the loop closes (401 before) |
| `sk_` | 200, unchanged |
| `pk_` + an allow-listed Origin | **200, unchanged** — the widget still works |
| `pk_` + wrong / absent Origin | **403, unchanged** — the delegate's Origin check still runs |
| garbage `sess_abc…` | 401 |
| `pk_` on `/documents/upload-url` | **403, unchanged** — `require_management()` did not widen |
| `sess_` on `/ingest` | **401** — *not* the 403 predicted; `/ingest` takes `AuthTenant`, so a session misses `api_keys` and never reaches the gate |
| tenant B's `conversation_id` | **404**, not 403 — invariant 8 |
| `RATE_LIMIT_PER_MINUTE + 1` asks with `sess_` | **429**, and the next `pk_` ask from the same tenant is also 429 — one bucket, spend answered |

**Invariant 16, read in the Network tab** — the only valid check. With `LLM_API_KEY` set to a canary,
the gateway's error body must be in the API log and **not in the response bytes**. Looking at rendered
text is the wrong test: the renderer discards it either way and would hide the failure.

**Browser, end to end**: tokens visibly arrive one at a time (a correct final answer proves nothing —
buffering would be byte-identical and pass every test; caught mid-stream at 697 vs 610 chars);
citations render *before* the first token with real filenames, numbered from `index`; the prose carries
no `[n]` markers; a refusal renders as a normal muted message, not red, with no LLM answer call in the
API log; a pronoun follow-up resolves, and stops resolving after "New chat".

## Found after shipping

**`max_tokens: 512` was shared with the model's reasoning.** Reported by the user as "Bahasa Indonesia
doesn't show chat" — an empty answer bubble under a rendered "Grounded in" list.

Bahasa Indonesia was not the cause. A reasoning model bills `reasoning_content` against the same
`max_tokens` budget as its prose, and `Delta` only reads `content`. Spend the budget on thinking and the
completion is *empty*: `finish_reason: "length"`, zero content deltas, no error, `done` yielded normally.
Reproduced against the gateway at `max_tokens=64` on both the JSON and streaming paths. At 512 a simple
question survived on ~80–180 reasoning tokens — a real margin, but thin enough that a longer document or
a cross-lingual question ate it.

Two fixes: `MAX_TOKENS = 4096` (a ceiling, not a target — the model stops when it is done), and
`ask.ts` now counts token frames and reports zero-with-sources rather than rendering silence.

**The lesson generalises, and is now in CLAUDE.md.** An empty answer is a *success* everywhere in the
stack: retrieval worked, nothing errored, nothing logged, nothing alerted. The client was the only
place that could notice, and it was rendering an empty bubble. All three of this phase's real bugs —
the leaked error body, the leaked cookie, the silent empty answer — were invisible to code review and
obvious to a capture.

## Not in this phase

- ~~**Serving `widget.js`**~~ — done in phase 7. Served from the API binary with `no-cache` + a strong
  ETag, so a fix now reaches every visitor rather than being frozen in a tenant's self-hosted copy.
- ~~**Widget citations**~~ — done in phase 7, and this entry is why it mattered: invariant 24's
  justification for the playground's JS spend — *"this is what your end users see"* — was **false on
  exactly the surface this phase added**, because the playground rendered citations the widget threw
  away. `widget.js` now renders them too, ported from `chat/sources.ts`, so the claim is true again.
- ~~**`/search`**~~ — gated and metered in phase 6 (`require_management`, so a `pk_` is refused, plus
  the tenant's rate-limit bucket). Still no UI, deliberately.
- **The `/dashboard` stub** — still the words `dashboard tenant`, still where every login lands.
