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
16. **Internal failure detail never reaches a client.** Unexpected errors are logged in full and
    answered generically. Caller errors describe the caller's mistake and nothing about internals.
17. **Passwords are Argon2id-hashed; session tokens are SHA-256-hashed. Neither is ever logged.**
    Same rule as invariant 14, extended from keys to human logins: a database dump is not a
    credential dump. `accounts` and `sessions` are global tables (no RLS), resolved on the plain pool
    *before* tenant context exists — a session lookup is what *establishes* that context. A session
    token carries the `sess_` prefix so it can never be confused with an `sk_`/`pk_` key.
18. **`/auth/register` and `/auth/login` are the only public credential endpoints, and both are rate
    limited.** Register is the single path that *creates a tenant* without the admin key, so its cap
    bounds abuse and `/embeddings` spend; login is a password oracle, throttled per email. Login
    failures are *uniform* — an unknown email and a wrong password return the identical 401, so the
    endpoint never reveals which emails exist (the same non-oracle rule as invariant 8). Sessions are
    Bearer tokens, **never cookies**: the `allow_origin(Any)` reasoning (see Security) depends on
    there being no cookie/CSRF surface, so the web BFF — not the API — owns any cookie.

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
- **`tenant.require_secret()?` is the first line** of any handler that ingests, uploads, lists or
  manages. Present on `/ingest`, `/documents` (both verbs) and the upload-url routes; correctly
  absent on `/ask` and `/ask/stream`; absent **as an outstanding gap** on `/search`.
- **Two auth principals, do not conflate them.** `AuthTenant` resolves an API key (`sk_`/`pk_`) to a
  tenant — the *machine* credential (a tenant's server, the widget). `SessionAuth` resolves a
  `sess_` token to an account + tenant — the *human* credential (the dashboard, the `/auth/*`
  routes). Both yield a `tenant_id`, so either can drive `db::tenant_tx()` and get RLS unchanged.
  `/auth/keys` lets a logged-in tenant mint/list/revoke its own keys — the self-serve equivalent of
  the admin-only `mint_key`; the two share `handlers::provision_tenant` / `insert_api_key` so they
  cannot drift. Revoke is scoped by `tenant_id` in the `WHERE` clause — that guard, not RLS (api_keys
  has none), is the isolation boundary for key management.
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
| `AuthTenant` / `AdminAuth` / `SessionAuth`, `hash_key`, `require_secret`, key + session token gen | `crates/api/src/auth.rs` |
| Self-serve accounts: register / login / logout / me / self-serve key mgmt; Argon2 hashing | `crates/api/src/accounts.rs` |
| `AppError` and the blanket `From` impl that makes `?` a 500 | `crates/api/src/error.rs` |
| Env vars and their defaults | `crates/api/src/config.rs` |
| Worker claim / status machine | `crates/worker/src/lifecycle.rs` |
| MinIO event parsing (the percent-encoding trap) | `crates/worker/src/event.rs` |
| Chunking | `crates/worker/src/chunk.rs` |
| Reaper — `UPLOAD_GRACE` and `PROCESSING_LEASE` are named constants; read them there | `crates/worker/src/reaper.rs` |
| PDF/text extraction, exit codes 2 and 3 | `sidecar/parser.py` |
| Embeddable widget | `widget/widget.js` |
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
