# Feature: Spend containment & gateway stability (phase 6)

> Status: **planned.** This is the Plan of Record — the design below has been audited against the code
> and against reqwest 0.12.28's vendored source. Two clauses of the original draft were wrong and are
> corrected here with the evidence inline; read *Known debt & traps* before writing any code, because
> the obvious implementation of the timeout half breaks every long answer.

## Context — why

Two leaks expose the platform to unbounded cost and unbounded resource use. Both are documented in
CLAUDE.md's *Known state & debt* as oversights rather than decisions, and this phase closes them.

### `/search` is neither gated nor metered

Verified: `handlers.rs:161` takes `tenant: AuthTenant`, calls **no gate**, and is not among the five
`rate_limit::check` call sites (`/ask`, `/ask/stream`, both upload-url routes, the deprecated
multipart `POST /documents`). Every search is a billed `/embeddings` call against `EMBEDDING_API_KEY`.

The exposure is invariant 15's containment failing. A `pk_` is printed in public page source and is
**expected to be stolen** — that is the design, and what makes it safe is that "it can only ask
questions". Asking questions is bounded by `rate_limit::check`. Searching is bounded by nothing.

**And invariant 15's literal claim is already false.** It says a `pk_` "may only ask questions."
`/search` is not a question — it is a raw retrieval API returning passages — and it accepts `pk_`
today. So this phase must either gate `/search` (making the invariant true again) or amend the
invariant to admit it. CLAUDE.md's *Security* section already calls the gate *"absent **as an
outstanding gap**"*, which points at the first. See **D1** below.

### Nothing bounds a hung gateway call

Verified: `llm.rs:79` and `common/src/embedding.rs:111` are both a bare `reqwest::Client::new()`, and
reqwest's default is **no timeout at all**. A gateway that accepts a connection and then stalls holds
the request task, its memory, and — on `/ask/stream` — an open SSE response, forever.

The only ceilings that exist today are `max_tokens` on a *well-behaved* gateway, and `ASK_TIMEOUT_MS`,
which lives in the **web BFF** and therefore protects the dashboard's browser and nothing else. **A
`pk_` widget calls the API directly, with no BFF in front, and has no ceiling anywhere.** That is the
case this phase exists for; the dashboard is already covered.

## Intended outcome

1. `/search` cannot be used to draw unbounded billed embedding calls.
2. Invariant 15's claim about what a `pk_` may do is true again, or amended to what is true.
3. Every outgoing gateway call — LLM and embedding, API and worker — fails within a bounded time.
4. **No legitimate long answer, and no legitimate large ingest, is broken by (3).** This is the
   constraint the phase is most likely to violate.

## Decisions to take before coding

**D1 — does `/search` get `require_management()`, or only the limiter?**

Recommendation: **both**. The gate restores invariant 15's literal claim, and `require_management()` is
the right shape — `/search` is a server-side retrieval API, so `sk_` or `sess_` is exactly its
audience. Risk is low: the only caller in the repo is the Postman collection (`widget/` and `web/` never
call it), so no shipped client breaks.

**Do not oversell the gate.** It is *not* a confidentiality fix. `/ask` returns `sources[].text` — the
passages themselves — to a `pk_` already, so a stolen key can still extract the corpus by asking
questions. The gate buys spend containment and an honest invariant. Nothing more, and the doc should
not claim more.

Per *Working here*, this is a behaviour change: **CLAUDE.md moves first, in the same commit.**

**D2 — is `/ingest` in scope?**

CLAUDE.md pairs them: *"`/search` accepts publishable (`pk_`) keys and is not rate limited; **`/ingest`
is also unmetered**."* `/ingest` requires `sk_` (`require_secret()`), so it is far less exposed — a
secret key is not expected to be stolen. But it is still an unbounded billed embedding call behind one
credential.

Recommendation: **add the limiter, exclude everything else.** One line, same bucket, closes the pair
CLAUDE.md names. Do not touch `/ingest`'s deeper debt — it is the largest in the system and a phase of
its own.

**D3 — one timeout value, or several?** See *Design*, below; the embedding client's two callers pull in
opposite directions and this cannot be one number.

## Design

### `/search` — the easy half

Add `actor.require_management()?` (per D1) and `rate_limit::check(&state, &actor.tenant_id).await?` as
the first lines of the handler, matching `/ask`'s shape. The existing tenant-wide bucket is correct and
deliberate: spend is per tenant, not per credential, so a search and an ask draw on one bucket. That is
the same reasoning invariant 27 rests on.

Gating means the signature moves from `AuthTenant` to `Actor` (to admit `sess_`), which is the same
one-word change phase 5 made to the ask routes. `Json` stays last.

### Gateway timeouts — the half that is not what it looks like

> **The original draft said: `.timeout(Duration::from_secs(ASK_TIMEOUT_MS))` on the reqwest client
> instantiation in `llm.rs` and `embedding.rs`. Do not do this. It is wrong three times over, and the
> most important way is silent.**

**Wrong #1 — a client-wide `.timeout()` kills every long streaming answer.**

From reqwest 0.12.28's own source, verbatim:

> `ClientBuilder::timeout` — *"Enables a **total request timeout**. The timeout is applied from when the
> request starts connecting **until the response body has finished**. Also considered a total deadline."*

`answer_stream` returns `resp.bytes_stream()` — the LLM's SSE body, consumed lazily over the whole life
of the answer. A total deadline therefore caps **how long an answer may be**, not how long the gateway
may stall. Set it to 60s and every answer still streaming at 60s dies.

The failure is worse than a truncation, because three things happen at once. The stream yields `Err` →
`handlers.rs` sets `failed = true` and emits the `error` frame → `ask.ts` reports the generic failure —
**and `append_turn` is skipped** (`if !failed && !answer.is_empty()`), so the turn silently vanishes
from history. Invariant 7 doing its job, on a failure we manufactured.

**This is the `client.ts`-versus-`stream.ts` trap, one layer down and in Rust.** The *Traps* table
already records the same shape in TypeScript: *"the 10s `AbortSignal` covers the body, not just the
headers."* Same mistake, same reason, different language.

The correct instrument is the one reqwest documents for exactly this:

> `ClientBuilder::read_timeout` — *"The timeout applies to **each read operation, and resets after a
> successful read**. This is **more appropriate for detecting stalled connections when the size isn't
> known beforehand**."*

That is a *stall* detector, which is what "a hung gateway" actually means. It bounds silence between
tokens, not the answer's length. So:

| Call | Instrument |
| --- | --- |
| `LlmClient::answer_stream` (SSE body) | `connect_timeout` + `read_timeout` **only** — no total deadline |
| `LlmClient::answer` (one JSON body) | the above **plus** a per-request total, via `RequestBuilder::timeout` |
| `EmbeddingClient::embed_one_batch` (one JSON body) | `connect_timeout` + `read_timeout` + a total |

`RequestBuilder::timeout` is the tool for the split — reqwest documents it as *"affects only this
request and overrides the timeout configured using `ClientBuilder::timeout()`"* — so one client can
serve both paths: build it with no total, and add one per request on the non-streaming calls.

**Wrong #2 — `ASK_TIMEOUT_MS` does not exist in Rust, and the unit is wrong.** It is a `web/` env var,
read in `web/src/lib/server/env.ts`, in **milliseconds**, default 120_000. `config.rs` has no timeout
field at all. `Duration::from_secs(120_000)` is **33 hours**.

**Wrong #3 — and it should not be shared even after conversion.** `ASK_TIMEOUT_MS` bounds the BFF's
wall clock for the dashboard's browser. The API's ceiling is a different bound protecting a different
client — the `pk_` widget with no BFF in front. They are independent, and CLAUDE.md's rule applies:
*do not restate values this file does not own.* New API-side config, named and defaulted in
`config.rs`, documented in README's env block.

### The embedding client is shared with the worker — D3

`common/src/embedding.rs` is `EmbeddingClient`, and its own module doc plus a *Traps* row pin that both
binaries use it. **So a timeout added in `EmbeddingClient::new` changes worker ingestion, not just the
API.** The draft treats `embedding.rs` as API-side; it is not.

The two callers pull opposite ways:

- `embed_one` — a single question, on `/ask`'s hot path. Wants a *tight* bound; a user is waiting.
- `embed_one_batch` — up to `EMBED_BATCH` inputs, in the worker, ingesting a PDF. Legitimately slow, and
  nobody is waiting.

One number cannot serve both: tight enough for a question is tight enough to fail a full batch, and a
failed batch is a document that does not index. The timeout must therefore be **per call site, not per
client** — which the `RequestBuilder::timeout` split above already affords. Read `EMBED_BATCH` from the
source when picking values; this file does not own it.

**The good news, and it shrinks the phase.** `EmbedError::Transport`'s doc comment already reads *"Could
not reach the endpoint, or the connection broke: DNS, TLS, **timeout**, reset"*, and `is_fatal()` returns
`false` for it — **retryable**. So the worker's fatal-vs-retryable contract already classifies a timeout
correctly, and correctly: a stalled gateway must never destroy a tenant's document. No worker change is
needed, and CLAUDE.md's *"an unclassified error in the worker is a bug"* is already satisfied. The
classification was written in anticipation of this.

## Verification plan

**Unit** — `cargo test`, `cargo clippy && cargo fmt`. Note what unit tests *cannot* reach here:
`Actor::from_request_parts` needs a database, so `/search`'s gate is provable only against a live stack.

**`/search`, live stack:**

| | |
| --- | --- |
| `sk_` on `/search` | 200 |
| `sess_` on `/search` | **200** — new, via `Actor` (D1) |
| `pk_` on `/search`, allow-listed Origin | **403** — new, and the point of D1 |
| `RATE_LIMIT_PER_MINUTE + 1` searches | **429** |
| then one `pk_` **ask** from the same tenant | **429** — one bucket; this is the spend claim, not an isolation claim |
| tenant A searches for a doc only B uploaded | no hits — invariant 1, unchanged |

**Timeouts — the stall test, which is the easy one:** point `LLM_BASE_URL` at a mock that accepts the
connection, returns headers, then sends nothing. Assert the call fails within the bound rather than
hanging. Repeat for `EMBEDDING_BASE_URL`. A mid-*stream* stall (some tokens, then silence) must abort
too — that is what pins `read_timeout` actually covering `bytes_stream()`, which is the one design claim
here taken from documentation rather than observation.

**Timeouts — the regression test, which is the one that matters and the draft omitted:**

| | |
| --- | --- |
| An answer that legitimately streams **longer than the total-timeout value** | **completes**, and `append_turn` records it |
| A PDF chunking to **more than `EMBED_BATCH` chunks** | ingests to `Ready` |

A stalled-gateway test passing proves nothing about either. If the timeout is built the way the draft
described, both of these fail and *only* these catch it.

**Dropped from the draft:** *"verify no Tokio tasks remain leaked/hanging."* Not assertable as written —
there is no task-count introspection without `tokio-console` or a metrics exporter, neither of which
exists here. The observable property is that the request *returns an error within the bound*; a task
that returns is not leaked. Do not write a test that cannot fail.

## Known debt & traps

> The draft said *"None identified yet; this is a cleanup and hardening phase."* That was the most
> dangerous line in it. A phase whose whole content is timeouts and gates is made of traps — the
> exposure is not new code, it is the code that already works and might stop.

| Don't | Do | Why |
| --- | --- | --- |
| `.timeout()` on the `LlmClient`'s reqwest client | `read_timeout` + a per-request total on `answer` only | It is a **total deadline including the body**. `answer_stream` *is* a long-lived body, so it caps answer length, not gateway stalls — and the truncated turn is then dropped from history by invariant 7 |
| Reuse `ASK_TIMEOUT_MS` | New API-side config in `config.rs` | It is a Node var, in **milliseconds**, bounding the BFF's browser. `from_secs` on it is 33 hours. The API's client is the `pk_` widget, which has no BFF in front |
| One timeout on `EmbeddingClient` | Per call site | It is shared with the **worker**. One value cannot serve a 1-input question and a full `EMBED_BATCH` ingest; the tight one fails documents |
| Assume a timeout is a new worker failure mode | Read `EmbedError::Transport` | Already classified retryable, deliberately — a stalled gateway must not destroy a document |
| Claim gating `/search` stops corpus extraction | Claim spend, and an honest invariant 15 | `/ask` hands `sources[].text` to a `pk_` already |

**Left standing, deliberately:**

- **A total timeout on `answer` bounds the JSON path but the read timeout is what bounds the stream.** So
  `/ask/stream` has no maximum duration — a gateway that trickles one token per read-timeout-minus-one
  streams forever. Bounded in practice by `MAX_TOKENS`, which is a real bound only on a well-behaved
  gateway. Accepting this is the price of not capping legitimate answers; the alternative is a token
  budget with a wall clock, which is a bigger design than this phase.
- **`/auth/keys` stays unmetered** — a session can mint unbounded keys. Not spend (the bucket is
  per-tenant), so it stays phase 4's open item.
- **`/ingest`'s document-model violation** is untouched by D2's limiter. Still the largest debt in the
  system.
