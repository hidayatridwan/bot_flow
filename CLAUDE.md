# bot_flow

Multi-tenant RAG customer-service chatbot SaaS. Tenants upload support documents (`pdf`/`txt`/`md`);
the platform parses, chunks, embeds and indexes them per tenant. End users ask questions through an
embeddable JS widget and get answers grounded **only** in that tenant's documents, with citations,
streamed over SSE.

> **This is a Rust Cargo workspace, not a Node project.** No `package.json`, no npm, no eslint.
> Reach for `cargo`, `sqlx` and `docker compose`.

`crates/api` (Axum HTTP server) · `crates/worker` (RabbitMQ consumer) · `crates/common` (shared
object-key contract **and the embedding client**) · `sidecar/` (Python `pypdf` extractor) ·
`widget/` (vanilla JS, no build step).

Backing services: Postgres 16, Qdrant, MinIO, RabbitMQ, Redis. Embeddings are an OpenAI-compatible
`/embeddings` call (`text-embedding-3-small`, 1536-dim, cosine), authenticated with
`EMBEDDING_API_KEY` — **a different key from `LLM_API_KEY`**, even when both point at the same
gateway. The LLM is any OpenAI-compatible `/chat/completions` endpoint.

## Commands

```bash
docker compose up -d      # five backing services (the binaries run on the host)
cargo run -p api          # http://localhost:3000 — also runs DB migrations on boot
cargo run -p worker       # ingestion consumer
cargo test                # inline #[cfg(test)] unit tests; no integration suite
cargo clippy && cargo fmt # stock defaults, no config files
```

Rust is pinned to 1.95.0 (`rust-toolchain.toml`).

## Invariants that must never break

Breaking one is not a bug to be weighed against other bugs — it is a product failure.

**Tenancy**

1. **Every Qdrant search is filtered by tenant.** `.filter(tenant_filter(&tenant.tenant_id))` is not
   optional. A search without it returns other customers' documents. If you write
   `QueryPointsBuilder::new(COLLECTION)` with no `.filter(...)`, the change is wrong — there is no
   read path that legitimately spans tenants.
2. **Every query on `documents` / `conversations` / `messages` goes through `db::tenant_tx()`.**
   Postgres RLS denies by default, so a forgotten tenant scope silently returns zero rows — or, on a
   dirty pooled connection, the wrong tenant's. `tenants` and `api_keys` are global: they are the
   tenancy registry itself, and are correctly queried on the plain pool.
3. **The storage key is the authorisation boundary of an upload.** A presigned URL authorises exactly
   one key, so the key *is* the permission. Hence the tenant-slug regex, enforced in both the
   application and a DB `CHECK` — a tenant named `a/../b` could mint a URL escaping into a
   neighbour's prefix. Each object key is unique across the system.

**Answering**

4. **An answer is grounded in retrieved context, or it is refused.** If nothing clears the relevance
   floor, the system returns a canned response and **does not call the LLM at all**. This bot answers
   *as the tenant's business*: a hallucinated refund policy is worse than an admission of ignorance,
   because the customer acts on it and the tenant is held to it.
5. **The model may only use the passages it is given**, and is forbidden from writing citation
   markers into its prose. The numbering exists for the machine; citations are returned as structured
   data alongside the answer.
6. **Chunks and questions must be embedded by the same model, through the same endpoint.** A
   collection may never hold vectors from two models: their coordinate spaces are unrelated, so a
   cosine score across them is noise that still looks like a number. Changing `EMBEDDING_MODEL`
   invalidates every stored vector. Nothing errors — retrieval silently degrades. A correctness rule
   wearing the costume of a configuration detail.
   (`text-embedding-3-small` is *symmetric*: it takes no `passage: ` / `query: ` prefixes. Those were
   an E5 artifact and were deleted with it. Re-adding them embeds the literal words into every vector.)
7. **A conversation turn is recorded only once an answer exists.** Otherwise a failed request leaves a
   dangling question, and the next question's rewrite reasons over it.
8. **An unknown conversation and another tenant's conversation are indistinguishable** — both 404.
   Returning 403 for one would make the endpoint an oracle for which IDs exist.

**Ingestion**

9. **Indexing the same document twice is a no-op, not a duplication.** The vector id is a
   deterministic UUIDv5 of (document, chunk index), so re-indexing overwrites in place. The worker
   deletes every existing vector for the document first, because a re-parse yielding *fewer* chunks
   would otherwise strand the old tail as orphans that still match searches. This is what makes
   redelivery safe.
10. **A document is claimed by exactly one worker.** A row lock plus a status check is the entire
    deduplication story. A second delivery finds the document finished with an identical fingerprint
    and skips; a *different* fingerprint means the client overwrote the file, so it is re-indexed.
11. **Upload size cannot be enforced at upload time.** A presigned signature covers method, key and
    expiry — **not body length**. The cap is enforced after the fact, by the worker, when the event
    arrives. Oversize documents are quarantined and their bytes deleted. Do not "move this check
    earlier" — there is no earlier.
12. **Every upload has a document record before it has a URL.** The reverse — a record whose upload
    never arrived — is expected, and is settled by the reaper.
13. **There is no upload-completion callback.** Storage announces the upload itself, so a client can
    neither forget to call it nor forge a call to it.

**Credentials**

14. **API keys are stored as SHA-256 hashes and never logged.** The raw key is shown exactly once, at
    mint. No secret, token or `.env` value belongs in any tracked file, comment, log or commit.
15. **A publishable key is chat-only and bound to an origin.** `sk_` lives on the tenant's server and
    may do everything; `pk_` is printed in public page source, may only ask questions, and only from
    an allow-listed `Origin`. A `pk_` key is *expected* to be stolen — that containment is the whole
    design. `/admin/*` is guarded by `ADMIN_API_KEY` (a deployment secret, not a DB row) because
    those are the operations that *create* DB rows.
    **The allow-list is that containment, so it is validated at mint, not merely stored.** An origin
    is compared to the `Origin` header by *string equality*, so a value that is not in a browser's
    canonical form is not lax — it is dead, and the key 403s forever. `auth::normalize_origin`
    canonicalises (lowercase, no trailing slash, default port stripped) and rejects what can never
    match; `handlers::checked_origins` enforces it inside `insert_api_key`, so the admin and
    self-serve mint paths cannot diverge on it. A **publishable key with an empty allow-list is
    refused** (422): it is not a permissive key, it is one that can answer from nowhere. `null` is
    never allow-listable — it is what every `file://` page and sandboxed iframe sends.
    The management gate has since widened to admit a *session* (invariant 23) — it has **never**
    widened for `pk_`, and must not. Widening it there deletes the containment above, which is the
    only thing that makes a public, stealable key safe to print.
16. **Internal failure detail never reaches a client.** Unexpected errors are logged in full and
    answered generically. Caller errors describe the caller's mistake and nothing about internals.
17. **Passwords are Argon2id-hashed; session tokens are SHA-256-hashed. Neither is ever logged.**
    Same rule as invariant 14, extended from keys to human logins: a database dump is not a
    credential dump. `accounts` and `sessions` are global tables (no RLS), resolved on the plain pool
    *before* tenant context exists — a session lookup is what *establishes* that context. A session
    token carries the `sess_` prefix so it can never be confused with an `sk_`/`pk_` key.
    **That prefix is load-bearing, not decoration.** `Actor` dispatches on it to choose which table
    to resolve a bearer token against — `sessions` or `api_keys`. The two are disjoint, so a token
    sent to the wrong one simply misses and 401s. Rename or drop the prefix and every session
    resolves against `api_keys`, misses, and the whole dashboard 401s at once.
18. **`/auth/register` and `/auth/login` are the only public credential endpoints, and both are rate
    limited.** Register is the single path that *creates a tenant* without the admin key, so its cap
    bounds abuse and `/embeddings` spend; login is a password oracle, throttled per email. Login
    failures are *uniform* — an unknown email and a wrong password return the identical 401, so the
    endpoint never reveals which emails exist (the same non-oracle rule as invariant 8). Sessions are
    Bearer tokens, **never cookies**: the `allow_origin(Any)` reasoning (see Security) depends on
    there being no cookie/CSRF surface, so the web BFF — not the API — owns any cookie.
19. **The uniform login failure must stay uniform in the UI.** Invariant 18 buys the non-oracle
    property at the API; the web app can hand it straight back. Under the *email* field, "invalid
    email or password" reads as *this email is wrong*; under *password*, as *this email exists, but
    the password is wrong*. So a login 401 is **form-level, always** — `error-map.ts` returns no
    field for it, and `error-map.test.ts` pins that. The two register **409s** are the mirror case:
    they share a status and are told apart **only** by their message, and they must land on
    different fields. An invariant is not enforced where it is written; it is enforced where it is
    displayed.
20. **The browser never holds a session token.** `web/` is a BFF: the `sess_` token lives in an
    `httpOnly` cookie on the *web* origin and is forwarded to the API as `Authorization: Bearer`
    from the server. `locals.session.token` is therefore never returned from a `load` — `locals`
    keeps the credential (`session`) and the identity (`user`) in separate fields precisely so a
    careless `return { user }` cannot leak it. The API itself stays cookie-free, which is what keeps
    invariant 18's CORS reasoning sound.
21. **An API outage is not a logout.** `hooks.server.ts` deletes the session cookie on a **401**
    only. A 5xx or an unreachable API leaves the cookie in place and merely renders the visitor as
    logged out for that request — otherwise a thirty-second blip silently signs out every user, and
    they cannot tell a dead session from a dead backend. This is the whole reason `ApiError.kind`
    distinguishes `client` / `server` / `transport` instead of just carrying a status.
22. **The one-time `sk_` shown at register is unrecoverable.** It reaches its reveal page in a
    5-minute `httpOnly` flash cookie that is read *and deleted* in the same request — never a query
    param (browser history, `Referer`, access logs) and never `localStorage`. Refreshing that page
    therefore loses the key, which is correct: it mirrors the API's own promise rather than papering
    over it. `POST /auth/keys` is the recovery path, and it is reachable: `/keys` in the dashboard
    mints, lists, revokes and edits. That page is what makes this invariant honest rather than a
    promise about an endpoint no user can call — the reveal alert links straight to it.
23. **The document-management routes take an `sk_` *or* a `sess_`, and never a `pk_`.** This is a
    consequence of invariant 22, not a convenience: the dashboard has no key to present. The one-time
    `sk_` is gone the moment its reveal page renders, and `GET /auth/keys` returns hashes. So the
    session is the *only* credential the BFF holds, and a dashboard that cannot list a document is
    not a dashboard. The alternatives were storing an `sk_` server-side (which contradicts invariant
    14's whole stated trade — hash, don't encrypt) or minting a key per page load.
    `Actor` is the union principal that expresses this, and `require_management()` is its gate:
    `Secret | Session` pass, `Publishable` is refused with a 403. `AuthTenant::require_secret()`
    still exists and still guards what stays key-only — `/ingest` and the deprecated multipart
    `POST /documents`. Both extractors yield a `tenant_id` and nothing else reaches the database, so
    **RLS is keyed on the string, not on how the string was obtained** — isolation is identical
    whichever credential arrived.
24. **A presigned URL in the browser is a capability, not a credential — and it does not break
    invariant 20.** The dashboard uploads by asking its own origin for a URL (server-side, with the
    session) and then PUTting the bytes *straight to MinIO*. The session never leaves Node; what the
    browser holds authorises one object key, one method, for one TTL. That is what a presigned URL
    *is*, and it is why `POST /documents/upload-url` exists at all.
    Two consequences, both easy to "fix" wrongly. **The PUT carries no `Authorization` header and no
    cookies** — the signature is in the query string, and MinIO rejects a request bearing both.
    **Uploading therefore requires JavaScript**, and that is architectural, not laziness: a multipart
    `<form>` action would proxy the bytes through Node, which is precisely the deprecated
    `POST /documents` we are deleting, rebuilt one layer up. Reading the list keeps the no-JS
    guarantee; only the write path spends it. `upload.test.ts` pins the header assertions, because a
    leak there would still upload fine and nothing else would notice.
25. **Re-minting an upload URL is only safe for the *same file*.** `refresh_session` re-signs the
    row's existing `object_key`, whose extension was fixed from the original filename at
    `create_session` — it takes no filename and revalidates nothing. Re-mint for a different file
    type and the bytes land at `original.pdf` while the sidecar, which dispatches on the suffix,
    fails a perfectly good file — and the user is told *their* document is broken. So the client
    re-mints in exactly one place: a mid-flight `403` (the signature outlived a slow upload) where it
    still holds the same `File`, so the extension provably cannot have changed. An `expired` row on a
    cold page load mints a **fresh** row instead. The endpoint is under-specified — it should either
    take a filename and revalidate, or stop embedding the extension in the key.
26. **A key's allow-list is editable; its `kind` and its hash are not.** `PATCH /auth/keys/{hash}`
    changes `allowed_origins` and nothing else. Adding a domain must not mean minting a new key: a
    `pk_` is public and expected to be stolen, so rotating it to add `www.` buys nothing, while
    forcing a site-wide `<script>` edit for a one-word change is how tenants end up begging for a
    wildcard — which would delete invariant 15's containment. `kind` stays immutable because flipping
    it would silently turn a published key secret, or a secret key public, under an unchanged snippet.
    The `tenant_id` in the `WHERE` clause is the isolation boundary (`api_keys` has no RLS), and a
    foreign hash **404s like an unknown one** — same non-oracle rule as invariants 8 and 18.

## Tenant isolation — the three layers

Any one layer would suffice on a good day. All three exist because a good day is not something to
depend on: one forgotten filter in one handler must not be enough to leak a customer's documents.

**Layer 1 — the Qdrant filter (application).** The `tenant_id` payload field is a keyword index with
`is_tenant(true)`, created in `ensure_collection()` **before any ingest happens**. That flag makes
Qdrant's HNSW graph filter-aware, so search is structured per-tenant rather than scanning globally
and discarding foreign hits afterwards. **Adding the index after data exists does not retroactively
restructure the graph.** The ordering is correctness, not style.

**Layer 2 — RLS in Postgres (database).** `documents`, `conversations` and `messages` have RLS
enabled *and forced* — `FORCE` matters because a plain policy does not apply to the table owner.
The tenant is bound per transaction with `set_config('app.current_tenant', $1, true)`, **never
`SET LOCAL`**, for two reasons:

1. Only `set_config` accepts a **bound parameter**. `SET LOCAL app.current_tenant = '...'` would
   require interpolating the tenant id into SQL — an injection vector in the one place that must
   never have one.
2. The third argument `true` means *transaction-local*: it resets on commit or rollback. That is what
   makes it safe with a connection pool. A session-level setting would **leak onto the next request
   that borrowed the same pooled connection**, silently handing it the previous tenant's identity.

Because the policy compares against a setting that is absent by default, a forgotten scope yields
zero rows, not everything. It fails closed.

**Layer 3 — a non-superuser runtime (deployment).** Postgres superusers bypass RLS entirely, so the
runtime connects as `app_user`. The admin pool exists to run migrations at API startup and is closed
immediately afterwards, so it cannot be reached for by a well-meaning refactor.

**The corollary trap:** a cross-tenant `UPDATE` under RLS **does not error — it matches zero rows and
reports success.** Any maintenance operation iterates tenant by tenant, as `reaper.rs` does. Silence
is not confirmation. When a query mysteriously affects nothing, suspect RLS before your `WHERE`.

## Traps

Each of these exists in, or nearly slipped into, this codebase.

| Don't | Do | Why |
| --- | --- | --- |
| Remove `tokio-executor-trait` / `tokio-reactor-trait` because they look unused | Leave them | They force `lapin` onto our Tokio runtime, wired in at `Connection::connect`. Without them it spawns a second runtime |
| Slice text by byte offset | Index over `chars` | Panics on any non-ASCII document — and the model is multilingual |
| Wrap an embedding call in `spawn_blocking` | `.await?` it like any other request | Embedding is a network call now, not local CPU. `spawn_blocking` would park a blocking thread on a socket. The old advice was the exact inverse — it is in the git history, not here |
| Put `Json` / `Multipart` first in a handler signature | Body extractor **always last** | `FromRequestParts` extractors must precede it. Otherwise you get an opaque `Handler` trait-bound error that says nothing about argument order |
| `?` on a caller-caused failure | `AppError::client(...)` | A bare `?` **always** yields a 500. Rule of thumb: `?` for what the caller could not have prevented, `AppError::client` for what they could |
| Return `400` for a bad field value | `422` if the body parsed as JSON | `400` is for a malformed body, unsupported extension, bad `kind`. Pinned by unit tests at the bottom of `handlers.rs` |
| Renumber `sources[].index` | Leave it 1-based | The model is forbidden from emitting citation markers, so `index` is the *only* way a client maps an answer back to a passage |
| Guess at an unparseable MinIO event key | Reject it | Keys arrive **percent-encoded** (`tenants%2Facme%2F…`), and a space may be `%20` **or** `+`. It is a schema we do not own |
| Bulk `UPDATE` across tenants | Loop per tenant | RLS matches zero rows and *reports success* |
| Version a dep in a member crate | `[workspace.dependencies]` | Two versions of `uuid` across the binaries produce different point ids — surfacing as bad search results, not a build error |
| Give the api and worker their own embedding code | Both call `common::embedding::EmbeddingClient` | Model name, request shape and `EMBEDDING_DIM` are defined once. Two copies drift, and drift here means the two binaries write vectors the other cannot search |
| Extend `POST /documents` (multipart) | `POST /documents/upload-url` | Deprecated; buffers whole files in API memory |
| Mirror `extension_of` in TS with `split('.').pop()` | Reject when the last dot is at index `<= 0` | It is `Path::extension()`: `.pdf` is a dotfile with **no** extension, `..pdf` has one. The naive version accepts `.pdf` and the API then 400s it. Pinned both sides — `key.rs::dotfiles_have_no_extension` and `documents/schema.test.ts` |
| Render `created_at` with `new Date(s)` | Normalise the offset first | Postgres `timestamptz::text` is `2026-07-16 11:39:20+00` — a space, and a 2-digit offset. ISO wants `T` and `+00:00`. `Date` returns **Invalid Date** silently, so the raw string reaches the UI. See `documents/format.ts` |
| Add a `<form action>` to the upload card | Leave it JS-only | A multipart action proxies bytes through Node — the deprecated route, one layer up (invariant 24) |
| Store an `allowed_origins` entry as the tenant typed it | `auth::normalize_origin` first | It is matched against `Origin` by string equality. `https://acme.com/`, `HTTPS://Acme.com` and `https://acme.com:443` all *look* right, mint fine, and never match — the key 403s forever with nothing in any log to say why |
| Render the embed snippet from any key you are handed | `embedSnippet` refuses a non-`pk_` | The snippet is designed to be pasted into a public page. An `sk_` there is invariant 15 inverted |

The sidecar signals failure with **exit codes, not stderr**: `2` = unreadable, `3` = unsupported type.
The worker classifies these into fatal-vs-retryable. It is a cross-language contract and neither side
documents the other — change one, change both. An unclassified error in the worker is a bug: if you
cannot say whether a failure is fatal or retryable, that is the design question to answer before
writing the code.

## Security

- **Hash, don't encrypt.** Keys carry ~244 bits of entropy (two v4 UUIDs) and only their SHA-256 hex
  reaches the database. An encrypted key can be decrypted by whoever holds the key-encryption key,
  which is on the same machine; a hash cannot be reversed, so a database dump is not a credential
  dump. The cost — we can never show a key again — is the intended trade.
- **A gate is the first line of any handler that ingests, uploads, lists or manages** — which gate
  depends on who may legitimately reach it:
  - `actor.require_management()?` — `sk_` **or** `sess_`, never `pk_`. On `GET /documents` and both
    upload-url routes: the tenant's own server *and* the dashboard reach these (invariant 23).
  - `tenant.require_secret()?` — `sk_` only. On `/ingest` and the deprecated multipart
    `POST /documents`. Both are paths we are not extending, so neither gets a session.

  Correctly absent on `/ask` and `/ask/stream`, which `pk_` is *meant* to reach; absent **as an
  outstanding gap** on `/search`.
- **Two auth principals, do not conflate them.** `AuthTenant` resolves an API key (`sk_`/`pk_`) to a
  tenant — the *machine* credential (a tenant's server, the widget). `SessionAuth` resolves a
  `sess_` token to an account + tenant — the *human* credential (the dashboard, the `/auth/*`
  routes). Both yield a `tenant_id`, so either can drive `db::tenant_tx()` and get RLS unchanged.
  `/auth/keys` lets a logged-in tenant mint/list/revoke its own keys — the self-serve equivalent of
  the admin-only `mint_key`; the two share `handlers::provision_tenant` / `insert_api_key` so they
  cannot drift. Revoke is scoped by `tenant_id` in the `WHERE` clause — that guard, not RLS (api_keys
  has none), is the isolation boundary for key management.
  **`Actor` is their union, and it does not conflate them.** Both extractors stay intact and
  independently usable; `Actor` exists only for the routes both may legitimately reach (invariant
  23), and it *widens* nothing on its own — `require_management()` does the deciding. It picks its
  delegate by the token's prefix, one table and one query, so a `sess_` is never looked up in
  `api_keys` nor an `sk_` in `sessions`. A `kind` the DB `CHECK` should have made impossible fails
  closed with the same 401 as an unknown key, so it is not an oracle either.
- **`allow_origin(Any)` is deliberate. Read this before "fixing" it.** CORS is a *browser* mechanism,
  not a server authorization mechanism — it cannot stop curl. The real check is the publishable key's
  `allowed_origins` list, enforced server-side in `AuthTenant` regardless of what the browser sent.
  And no cookies or session state are used, so there is no CSRF surface for a restrictive policy to
  defend. Tightening it would break every tenant's widget the moment they add a domain, while
  protecting nothing. Origin policy belongs in the per-key allow-list, because it is per tenant.
- Content types are derived from the extension we validated, **never taken from the client** — a
  client-supplied type would be a lie we then stored.

## Where things live

| Concern | File |
| --- | --- |
| Object-key contract (the upload boundary) — densest tests in the repo | `crates/common/src/key.rs` |
| Embedding client, `EMBEDDING_DIM`, batching, `EmbedError` — shared by both binaries | `crates/common/src/embedding.rs` |
| Routes, CORS layer, RabbitMQ connect | `crates/api/src/main.rs` |
| Handlers, Qdrant search, SSE stream | `crates/api/src/handlers.rs` |
| `tenant_tx()` and the two pools | `crates/api/src/db.rs` |
| `AuthTenant` / `AdminAuth` / `SessionAuth` / `Actor`, `hash_key`, `require_secret` vs `require_management`, the `sess_` prefix dispatch, `normalize_origin` (next to the `Origin` check it must agree with), key + session token gen | `crates/api/src/auth.rs` |
| Self-serve accounts: register / login / logout / me / self-serve key mgmt; Argon2 hashing | `crates/api/src/accounts.rs` |
| `AppError` and the blanket `From` impl that makes `?` a 500 | `crates/api/src/error.rs` |
| Env vars and their defaults | `crates/api/src/config.rs` |
| Worker claim / status machine | `crates/worker/src/lifecycle.rs` |
| MinIO event parsing (the percent-encoding trap) | `crates/worker/src/event.rs` |
| Chunking | `crates/worker/src/chunk.rs` |
| Reaper — `UPLOAD_GRACE` and `PROCESSING_LEASE` are named constants; read them there | `crates/worker/src/reaper.rs` |
| PDF/text extraction, exit codes 2 and 3 | `sidecar/parser.py` |
| Embeddable widget | `widget/widget.js` |
| Web BFF hinge — session cookie → `GET /auth/me` → `locals` | `web/src/hooks.server.ts` |
| Typed API client: `ApiResult`, the JSON-vs-`text/plain` split, timeouts | `web/src/lib/server/api/` |
| Session + one-time-key cookies; `requireUser` vs `requireSession` | `web/src/lib/server/auth/` |
| Login-401 and the two register-409s → which field (invariant 19) | `web/src/lib/features/auth/error-map.ts` |
| The TS mirrors of the Rust validators — drift here 422s/400s the user | `web/src/lib/features/{auth,documents}/schema.ts` |
| Browser → MinIO upload; the presigned-URL-is-not-a-credential boundary (invariant 24) | `web/src/lib/features/documents/upload.ts` |
| Origin validation + the mint/PATCH rules, shared by admin and self-serve | `crates/api/src/handlers.rs` (`checked_origins`, in `insert_api_key`) |
| The embed snippet, and its refusal to carry an `sk_` | `web/src/lib/features/keys/embed.ts` |
| Status → user-facing copy; where invariant 16 is enforced *in the UI* | `web/src/lib/features/documents/status.ts` |
| The only BFF route a browser fetches as JSON (mint + re-mint) | `web/src/routes/(authenticated)/documents/upload-url/+server.ts` |
| Migrations — forward-only, run at API startup on the admin pool, which is then closed | `crates/api/migrations/` |

## Known state & debt

Honest inventory. Each entry states the impact, not merely the fact.

- **`POST /ingest` violates the document model.** It writes vectors with random ids and **no
  `document_id` payload**. So: re-ingesting the same text *duplicates* the vectors (invariant 9 does
  not hold on this path); the chunks surface in answers with an empty document reference, so a client
  cannot attribute the citation; and they belong to no record, so they can never be listed,
  re-indexed or removed. They are permanent. Isolation *is* preserved — the tenant tag is written and
  the filter applies — so this is a data-lifecycle hole, not a leak. **The largest single piece of
  debt in the system.** Demo and testing convenience; not a supported path. Do not build on it.
- **There is no delete path for a document** — record, vectors and bytes persist forever. A "delete my
  data" request cannot presently be honoured. For a product holding customers' support documents this
  is a compliance gap, not a missing feature. Designing it means deciding the order of operations
  across three stores and what happens when a step fails halfway.
  **The dashboard now makes this visible**: `/documents` lists every row with no way to remove one,
  and `expired`/`failed` rows accumulate in the tenant's own view.
- **`GET /documents` has no pagination.** It returns the tenant's entire table, every call, fully
  materialised. Fine for a new tenant; a real problem at scale, and the dashboard polls it. The
  polling backs off to a 15s ceiling precisely because this query is unbounded — that is a mitigation,
  not a fix.
- **`failed` conflates "your file is broken" with "our worker died".** `mark_failed` writes the
  parser's stderr; the reaper writes `'processing lease expired; worker presumed dead'`. Both land in
  the `error` column, which **no endpoint exposes** — correctly, since invariant 16 forbids shipping
  either string to a client. The cost is that the UI cannot tell a tenant whether to re-upload or
  wait, so its copy names both causes. The fix is for the worker to write a *classified* reason code
  alongside the raw text, which the API could then expose safely. Until then a `failed` badge is
  honest but not actionable.
- **`POST /documents` (multipart proxy) is deprecated** — it buffers whole files in API memory. Use
  `POST /documents/upload-url`. It gets deleted along with `crates/api/src/queue.rs` and the worker's
  `consume_legacy` / `LEGACY_QUEUE`. Do not add features to it. Do not add callers.
- **`/search` accepts publishable (`pk_`) keys and is not rate limited; `/ingest` is also unmetered.**
  This was an unmetered-*CPU* oversight while embedding ran locally. It is now an unmetered-**spend**
  exposure: every search and every question is a billed `/embeddings` call against `EMBEDDING_API_KEY`.
  A `pk_` key is printed in public page source and is *expected to be stolen* (invariant 15) — the
  containment for that theft was "it can only ask questions", which no longer bounds the cost. Rate
  limiting `/search` is a cost control, not a nicety. Still believed an oversight, not a decision.
- **The isolation guarantee is untested.** No automated test asserts that RLS actually denies a
  cross-tenant read. Invariant 1 is the system's most important promise and it rests on code review
  alone. There is no CI and no integration suite; tests are unit tests over pure functions. Highest-
  value missing tests, in order: cross-tenant denial; concurrent claim of one document by two workers;
  origin rejection for `pk_` keys; correct fatal-versus-retryable classification.
- **Vector storage has no migration path.** Changing the embedding model, its dimension, or the
  chunking parameters invalidates every stored vector, and there is no rollback — only recreating the
  collection and re-indexing every document of every tenant. A **partially** re-indexed collection
  produces quietly degraded retrieval with no error anywhere. Any such change is a migration project,
  not a configuration change.
  This was paid, not solved, at the `MultilingualE5Small` → `text-embedding-3-small` cutover
  (384 → 1536 dim): all vectors and all `documents` rows were truncated and re-uploaded. Note the
  second half of that — `ensure_collection()` **early-returns when the collection exists** and will
  not rebuild it, and invariant 10 *skips* a redelivered document whose fingerprint is unchanged. So
  dropping the collection alone leaves it permanently empty, with no error. The document rows must go
  too. Whoever changes the model next will hit both.
- The DB permits an `uploaded` document status that **no code path ever assigns**. A vestige. Either
  give it meaning or drop it from the constraint — an unreachable state is a trap for the next reader.
- **No `.env.example`**, though `.gitignore` expects one. A new contributor reconstructs the required
  configuration from `config.rs`. Names only, values blank; its absence is pure friction.

## Working here

- **Comments explain *why*, never *what*.** Record the trap or the trade-off, not a restatement of the
  line below it. Most of this file is comments that outgrew their source.
- **A behaviour change starts here.** Edit the invariant, then write the code — in the same commit.
  A document that quietly drifts from the code is worse than no document, because it is trusted.
- **Do not restate values this file does not own.** Chunk size and overlap, the relevance floor, the
  presign TTL, the upload cap, history depth, ports and the full `.env` block live in `README.md` and
  in the code. Duplicating them is how they drift. Point at the constant; don't copy it.
- Tests are inline `#[cfg(test)]` unit tests and must pass with **no backing services running**.
  Anything needing Postgres or Qdrant belongs in `crates/<crate>/tests/`, which does not exist yet —
  discuss before introducing one. Do not widen visibility just to test something.
