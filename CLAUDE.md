# bot_flow

Multi-tenant RAG customer-service chatbot SaaS. Tenants upload support documents (`pdf`/`txt`/`md`);
the platform parses, chunks, embeds and indexes them per tenant. End users ask questions through an
embeddable JS widget and get answers grounded **only** in that tenant's documents, with citations,
streamed over SSE.

> **Two projects, one repo, and the root is the Rust one.** Everything outside `web/` is a Cargo
> workspace: no `package.json`, no npm, no eslint — reach for `cargo`, `sqlx` and `docker compose`.
> `web/` alone is a Node project (SvelteKit, `bun`, eslint, prettier, vitest). Run the right tool in
> the right directory; a `cargo` command at `web/` or an `npm` command at the root is a sign you have
> the wrong half.

`crates/api` (Axum HTTP server) · `crates/worker` (RabbitMQ consumer) · `crates/common` (shared
object-key contract, the embedding client **and the chunker** — the index recipe) · `crates/eval`
(the retrieval bench) · `sidecar/` (Python `pypdf` extractor) ·
`widget/` (vanilla JS, no build step) · `web/` (SvelteKit BFF — the dashboard).

Backing services: Postgres 16, Qdrant, MinIO, RabbitMQ, Redis. Embeddings are an OpenAI-compatible
`/embeddings` call (`text-embedding-3-small`, 1536-dim, cosine), authenticated with
`EMBEDDING_API_KEY` — **a different key from `LLM_API_KEY`**, even when both point at the same
gateway. The LLM is any OpenAI-compatible `/chat/completions` endpoint.

## Commands

```bash
docker compose up -d      # five backing services (the binaries run on the host)
cargo run -p api          # http://localhost:3000 — also runs DB migrations on boot
cargo run -p worker       # ingestion consumer
cargo test                # inline #[cfg(test)] unit tests — offline, no services needed
cargo test -- --ignored   # the integration suite: needs `docker compose up -d` (phase 9)
cargo run -p eval         # the retrieval bench: real, billed embeddings; own collection (phase 10)
./scripts/backup.sh       # Postgres + MinIO. Qdrant is derived — `worker reindex` rebuilds it
./scripts/restore.sh DIR  # destroys current state; restart the api, then reindex (phase 13)
cargo clippy && cargo fmt # stock defaults, no config files
```

From `web/`, for the dashboard:

```bash
bun run dev               # http://localhost:5173 — needs the api running
bun run test              # vitest; pure units, no services
bun run check             # svelte-check, strict
```

`bun run lint` is `prettier --check . && eslint .`, and **prettier currently fails on ~208
pre-existing files** — nearly all of `lib/components/ui/` (vendored shadcn). So the command exits
non-zero on a clean checkout and eslint never runs behind it. Check your own files
(`npx prettier --check <path>`, `npx eslint <path>`) rather than reading the summary count, and do not
sweep the 208 into an unrelated diff.

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
   floor, the system returns a canned response and **does not call the LLM at all**. That floor is
   now a single **measured** number (`0.25`, `config.rs`) rather than the three disagreeing values it
   used to be, and it is applied **after** an over-fetch so it sharpens the context instead of
   shrinking it. Retune it with `cargo run -p eval`, never by intuition: 0.35 looks reasonable and
   costs recall@3 1.000 → 0.955. This bot answers
   *as the tenant's business*: a hallucinated refund policy is worse than an admission of ignorance,
   because the customer acts on it and the tenant is held to it.
5. **The model may only use the passages it is given**, and is forbidden from writing citation
   markers into its prose. The numbering exists for the machine; citations are returned as structured
   data alongside the answer.
   **It also answers in the language it was asked in**, which is a rule about *presentation*, not
   content — it loosens nothing above. Without it the model picks a language on its own and picks
   inconsistently: the same Indonesian corpus answered `siapa imam?` in English and
   `ceritakan tentang pengalaman kerja Imam` in Indonesian, because the passages were English and
   nothing said otherwise. An end user asking in their own language and being answered in another
   reads as a broken bot, and the tenant cannot fix it — the prompt is ours, not theirs.
   Note the seam this does **not** close: `NO_ANSWER` is a fixed English string, so a refusal
   (invariant 4) is still English whatever the question's language. The refusal path never reaches
   the model, so the prompt cannot reach it either; translating it would mean choosing a language
   without one to mirror.
6. **Chunks and questions must be embedded by the same model, through the same endpoint.** A
   collection may never hold vectors from two models: their coordinate spaces are unrelated, so a
   cosine score across them is noise that still looks like a number. Changing `EMBEDDING_MODEL`
   invalidates every stored vector. Nothing errors — retrieval silently degrades. A correctness rule
   wearing the costume of a configuration detail.
   (`text-embedding-3-small` is *symmetric*: it takes no `passage: ` / `query: ` prefixes. Those were
   an E5 artifact and were deleted with it. Re-adding them embeds the literal words into every vector.)
   **The rule is about the whole index recipe, not only the model.** Chunking strategy, `CHUNK_SIZE`
   and `CHUNK_OVERLAP` are inside this sentence: a collection may not hold chunks cut two different
   ways any more than vectors from two models, and the failure is identical — a score that is noise
   but still looks like a number. That is why the chunker, the embedding client and the collection
   name all live in `common`, and why the collection name is **versioned** (`documents_v2`). Changing
   any of them is a migration with a rollback, not a setting.
   **A chunk carries its provenance**: every indexed point has `document_id`, `chunk_index`,
   `char_start`, `char_end` and `created_at`. None of the last four can be reconstructed — the point
   id is a UUIDv5 hash and does not invert — and adding one later is a *second* full re-index, which
   is why they are written now and read by almost nothing yet. **As of phase 11 this holds on every
   path**: `/ingest` no longer writes vectors directly — it writes the text to MinIO as an object and
   lets the same worker index it, so there is exactly one recipe for turning text into vectors and
   exactly one shape of point in the collection.
7. **A conversation turn is recorded only once an answer exists.** Otherwise a failed request leaves a
   dangling question, and the next question's rewrite reasons over it.
8. **An unknown conversation and another tenant's conversation are indistinguishable** — both 404.
   Returning 403 for one would make the endpoint an oracle for which IDs exist.

**Ingestion**

9. **Indexing the same document twice is a no-op, not a duplication.** The vector id is a
   deterministic UUIDv5 of (document, chunk index), so re-indexing overwrites in place. **This
   sentence carried an unwritten exception for `/ingest` until phase 11** — that path wrote random
   ids, so re-ingesting duplicated. It does not any more. The worker
   deletes every existing vector for the document first, because a re-parse yielding *fewer* chunks
   would otherwise strand the old tail as orphans that still match searches. This is what makes
   redelivery safe.
10. **A document is claimed by exactly one worker, and the worker never resurrects a row it no longer
    owns.** A row lock plus a status check is the entire deduplication story. A second delivery finds
    the document finished with an identical fingerprint and skips; a *different* fingerprint means the
    client overwrote the file, so it is re-indexed.
    **`claim` skips a `deleting` row, and the two post-index transitions (`mark_ready`, `mark_failed`)
    fire only `WHERE status = 'processing'`.** Both halves guard the same hazard: the worker releases
    its row lock at claim time and only *then* parses, embeds and upserts, so for the whole life of an
    index it holds no lock. A delete that tombstones the row to `deleting` mid-index (phase 8), or a
    reaper that reclaims a stale lease to `failed`, must not be overwritten by a worker finishing late
    — an unguarded `mark_ready` would flip `deleting` back to `ready` and resurrect a document being
    erased, and the deferred-delete sweep, which looks for `deleting`, would never find it again. On a
    zero-row guard the worker returns "not mine to finish" and stops; the chunks it wrote are orphans
    the delete sweep clears by `document_id`. `mark_quarantined` is deliberately *not* guarded: it runs
    on the oversize path *before* the claim, so its row is never `processing`, and its only race is
    with the synchronous delete path, whose final `DELETE` is unconditional and wins regardless.
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
    **"Chat-only" is a gate, not a description of intent.** `/ask` and `/ask/stream` are the routes a
    `pk_` may reach; `/search` — a raw retrieval API, not a question — refuses it via
    `require_management()`. It *accepted* one until phase 6, which made this invariant's own first
    sentence false, and the answer was to gate the route rather than soften the claim. Note what the
    gate does and does not buy: `/ask` hands `sources[].text` to a `pk_` already, so this is not a
    confidentiality boundary and never was. It bounds **spend** — every search is a billed
    `/embeddings` call — and it makes the sentence above true.
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
    **Password reset tokens join this rule** (phase 16): `password_reset_tokens` is a third global,
    RLS-free table, the token is stored SHA-256 and carries its own `rst_` prefix, and it is never
    logged — not the token, and not the link containing it. A reset link is the most dangerous
    credential in the system, because redeeming one *takes* an account rather than merely opening
    it, and it arrives in an email nobody controls after delivery. Hence: single use, one hour, and
    redeeming one revokes **every session** the account had (the person resetting may be recovering
    from a compromise) and burns every other outstanding link. `rst_` must never become `sess_`,
    for the reason above and a worse one — `Actor` would then offer a password-changing credential
    to a code path that grants access.
18. **Every public credential endpoint is rate limited, and none of them is an existence oracle.**
    There are four: `/auth/register`, `/auth/login`, `/auth/password/forgot` and
    `/auth/password/reset`. Register is the single path that *creates a tenant* without the admin
    key, so its cap bounds abuse and `/embeddings` spend; login is a password oracle, throttled per
    email; forgot is throttled per email too; reset is throttled per token.
    **The non-oracle rule is what they share**, and it is the same rule as invariant 8. Login
    failures are *uniform* — an unknown email and a wrong password return the identical 401.
    `/auth/password/forgot` goes further and returns **`202` for everything**: a registered address,
    an unregistered one, and outright garbage. It deliberately does not even validate the address
    shape, because a `422` for a malformed one is a free "this shape is accepted" signal for a
    well-formed one. `/auth/password/reset` answers one `400` for expired, already-used and
    never-existed alike — one `UPDATE … WHERE used_at IS NULL AND expires_at > now()` covers all
    three, so the API genuinely cannot tell them apart and therefore cannot leak which it was.
    **The rule extends to *timing*, not just status codes.** A known address costing an SMTP
    conversation while an unknown one returned instantly would make the response *time* the oracle
    the status code is not — which is why delivery is spawned rather than awaited. Measured: known
    2.3–3.3ms, unknown 1.5–2.5ms, ranges overlapping. The residue is one `INSERT`, not a round trip.
    Sessions are Bearer tokens, **never cookies**: the `allow_origin(Any)` reasoning (see Security)
    depends on there being no cookie/CSRF surface, so the web BFF — not the API — owns any cookie.
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
    **That last sentence costs one word per outbound call, and was false until it was paid.**
    `event.fetch` defaults to `credentials: 'same-origin'`, and its notion of same-origin is a
    *hostname suffix* match: it injects the browser's whole cookie jar into the upstream request
    whenever `` `.<apiHost>`.endsWith(`.<webHost>`) `` — `localhost`→`localhost` in dev,
    `example.com`→`api.example.com` in production. It does this *after* our header object is built,
    so no amount of care in constructing headers prevents it and no assertion over them can see it.
    Every API call shipped `bf_session` to the API. It was never an auth hole — the API reads only
    `Authorization` — but the token landed in the API's access logs, and invariant 18's "no cookie
    surface, therefore no CSRF surface" rested on a fact that was not true. Both `client.ts` and
    `stream.ts` therefore pass **`credentials: 'omit'`**, and both pin it with a test, because this is
    invisible in review: the code reads correctly either way.
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
    `Actor` is the union principal that expresses this, and `require_management()` is its gate *on
    these routes*: `Secret | Session` pass, `Publishable` is refused with a 403. The extractor and the
    gate are two decisions, not one — `Actor` also carries the ask routes, where it deliberately gates
    nothing (invariant 27). Widening the *extractor* is not widening the *gate*, and this invariant is
    about the gate. `AuthTenant::require_secret()` still exists and still guards what stays key-only —
    `/ingest` and the deprecated multipart `POST /documents`. Both extractors yield a `tenant_id` and
    nothing else reaches the database, so **RLS is keyed on the string, not on how the string was
    obtained** — isolation is identical whichever credential arrived.
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
    guarantee; the write path spends it. `upload.test.ts` pins the header assertions, because a
    leak there would still upload fine and nothing else would notice.
    **The playground spends it a second time, and on different grounds — worth stating, because the
    two must not be confused.** Uploading has *no* no-JS design available. The playground does: a form
    action over `POST /ask` would work perfectly without JavaScript. So this one is **bought, not
    forced**, and what it buys is the only claim the page makes — *this is what your end users see*,
    and what they see is tokens arriving. A form-action playground would answer correctly and feel
    like a different product, hiding the exact property a tenant checks before going live. It costs
    nothing that previously worked: a new page, not a regression.
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
27. **The ask routes admit any authenticated principal of the tenant, and that is not a widening.**
    `/ask` and `/ask/stream` take `Actor` and call **no gate**: `Secret`, `Publishable` and `Session`
    all pass. Read that against invariant 15 before reaching for a gate.
    A `pk_` is printed in public page source and is *expected* to be stolen — and it **already reached
    these routes**, because "it can only ask questions" is precisely the containment that makes it safe
    to print. So the ask routes are, by design, the ones the *weakest* credential in the system may
    reach. A `sess_` costs an Argon2-verified password and expires; an `sk_` is stronger still.
    Admitting the stronger credentials to a route the weakest already reaches adds no exposure — which
    is the whole argument, and it is why this is not the mirror of invariant 23. That gate refuses
    `pk_`; this one refuses nothing, deliberately.
    **No gate is not no auth.** `Actor` still resolves the token against exactly one table, chosen by
    prefix, and on the `pk_` branch its `AuthTenant` delegate still enforces the `Origin` allow-list.
    Nothing about a publishable key's containment changed.
    **The trap:** a future reader will want to "secure" this by adding `require_management()`. That
    gate 403s every deployed widget — the one client these routes exist to serve — and it would be
    discovered by tenants, not by tests, because `Actor::from_request_parts` needs a database and the
    unit tests cannot see it.
    Spend is bounded by `rate_limit::check`, which keys on `tenant_id` and not on the credential, so a
    session cannot draw a token more than that tenant's own `pk_` already could. That is what answers
    the spend question this change would otherwise raise: the limiter that was already there.

**Erasure**

29. **Every indexed point is erasable by document.** For every point in the collection there is a
    `documents` row whose deletion removes it — no exceptions, no second write path. This is stated
    as an invariant rather than left as a property because it is exactly what a compliance question
    asks, and because the only way it breaks is by someone adding another way to write vectors that
    skips the row. That is precisely how it broke the first time: `POST /ingest` wrote points with
    random ids and no `document_id`, and CLAUDE.md carried them as *"permanent"* for eight phases.
    The fix was not a second ingestion path but the **absence** of one — `/ingest` now writes its
    text to MinIO and the ordinary pipeline indexes it.
    **A tenant is erasable too, and the record of an erasure outlives its subject** (phase 12).
    `DELETE /admin/tenants/{id}` removes vectors, objects and rows; access is revoked *first* so
    nothing can authenticate mid-erasure, and the vector sweep runs *twice* because a worker already
    indexing holds no lock and can upsert between them. Answers that quoted a deleted document are
    **redacted, not deleted** — the turn stays, its content becomes a tombstone — which is possible
    only because assistant turns now carry their sources in `metadata`.
    **`erasures` has no foreign key to `tenants` and no RLS, deliberately.** Every neighbouring table
    cascades on tenant deletion; an audit row that did the same would be destroyed by the erasure it
    records, in the same statement, and the destruction would look like diligence.
    **Any new store that retains tenant identity is inside this invariant's scope** and must either
    be erasable or must not hold the identity. Invariant 30 is that rule applied to the first such
    candidate — a metrics backend — and it chose the second option.
    Still not promised: turns written before phase 12 carry no provenance and cannot be found; there
    is no retention policy; the audit is a table an operator can edit, not a tamper-evident log; and
    `purge-unattributed` writes no audit row. Calling this "GDPR compliant" would be the kind of
    claim this file exists to prevent.

30. **No metric carries a tenant identifier.** A Prometheus series labelled `tenant="acme"` is a
    **fourth store**, outside every erasure guarantee invariant 29 makes — and it is the one store
    that cannot be fixed later. Prometheus is *designed* not to be deleted from: the admin delete
    API is off by default, a delete is a tombstone until compaction, and a remote-write copy
    (Thanos, Mimir, a vendor) may have no delete path you control at all. `DELETE /admin/tenants/{id}`
    returns counts as evidence of what it removed; with a tenant label it would be returning evidence
    for three stores while silently omitting a fourth. That is invariant 29's own failure mode —
    a partial erasure that looks like diligence.
    Three lesser reasons, each sufficient on its own: a tenant slug is customer-authored, and metrics
    are routinely shipped to a monitoring vendor and read by a wider audience than the database ever
    is; series are never garbage-collected on tenant deletion, so cardinality grows with the customer
    list forever; and `/metrics` would become an enumerable tenant registry for anyone with scrape
    access.
    **The rule that enforces this mechanically: every label value is a variant of a closed enum or a
    `const`, never a runtime string.** That single rule also keeps invariant 16 out of the label
    space — an error body as a label value is invariant 16 inverted *and* a cardinality bomb in one
    move — and it is checkable by reading the code rather than the data.
    **"Which tenant?" is `GET /admin/ops/tenants`**, read live from Postgres. It is safe precisely
    because it *retains nothing*: the answer is always current, needs no retention policy, and a
    tenant erased by invariant 29 vanishes from it in the same statement, with no extra code. Time
    series answer *"is something wrong"*; the live query answers *"who"*. The cost, accepted
    knowingly: you cannot ask who caused last Tuesday's spike, only who is causing this one.
    Hashing or pseudonymising the tenant id does **not** satisfy this. A stable pseudonym still
    survives erasure and still correlates with the live database; it is the same store wearing a
    smaller hat.

**Gateways**

28. **Every outgoing gateway call is bounded — and the bound on a streamed body is a *stall*, not a
    deadline.** The distinction is the whole invariant, because the obvious implementation inverts it.
    `reqwest`'s `ClientBuilder::timeout` is, in its own words, a *"total request timeout… applied from
    when the request starts connecting **until the response body has finished**."* `answer_stream`
    **is** a long-lived body — the LLM's SSE, consumed for the whole life of the answer. So a total
    deadline there does not bound a hung gateway; it caps **how long an answer may be**, and kills
    every legitimate answer still streaming when it fires. Worse, silently: the stream yields `Err`,
    `handlers.rs` sets `failed = true` and emits the `error` frame, and `append_turn` is then skipped —
    so invariant 7 correctly drops a turn from history that only our own timeout truncated.
    The instrument for a hang is `read_timeout`: *"applies to each read operation, and resets after a
    successful read… more appropriate for detecting stalled connections when the size isn't known
    beforehand."* That bounds silence between tokens, which is what "the gateway hung" actually means,
    and it is indifferent to how long the answer runs. So: `connect_timeout` + `read_timeout` on every
    client; a **total** only on the non-streaming calls, per request, via `RequestBuilder::timeout`.
    The bounds are named constants beside the code they bound (`llm.rs`, `common/src/embedding.rs`),
    not env vars — the same choice, for the same reason, as `MAX_TOKENS` and `EMBED_BATCH` next to
    them. A timeout is a correctness bound on our own resource use, not a deployment preference, and
    `EmbeddingClient` is shared by two binaries that configure themselves differently.

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

**The integration harness connects as `app_user` and *proves* it, on every run.** This belongs here
rather than in the invariant list because it is a property of the tests, not of the product — but it
is what makes the tests of everything above worth anything. `guard_not_superuser` asserts
`NOT rolsuper` and aborts before the first assertion. Without it, a harness pointed at the migration
role would assert "tenant B cannot see tenant A's document", pass, and have tested **nothing**,
because the query it ran was never subject to the policy. Verified once by hand, that stays
re-breakable forever by anyone editing `.env`; asserted every run, it does not.

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
| Ship a `widget.js` fix and trust the served bytes to update | Ensure the build recompiles — advance its mtime or `cargo clean` | `widget.rs` embeds it with `include_str!`, a **compile-time** read cargo fingerprints by mtime. A byte change with an unchanged mtime (a `mv` that preserves it, some checkout patterns) leaves stale bytes in a reused binary, the ETag never moves, and the fix silently does not ship — the very staleness the served route exists to end, one layer lower. Clean CI builds are immune; incremental/container-layer ones are not |
| "Secure" `/ask` with `require_management()` | Leave it ungated — that is invariant 27 | It is the one route a `pk_` exists to reach. A gate there 403s every deployed widget, and no unit test catches it: `Actor::from_request_parts` needs a database, so tenants find it, not CI |
| Call the API through `event.fetch` without `credentials: 'omit'` | Pass it, and pin it | Kit counts a *hostname suffix* match as same-origin and injects the browser's cookie jar — `localhost`→`localhost`, `example.com`→`api.example.com`. It happens after your headers are built, so the code reads correctly and still ships `bf_session` to the API (invariant 20) |
| Reach for `client.ts` to fetch a stream | `server/api/stream.ts` | `parseResponse` opens with `await res.text()` — it consumes the body by construction — and the 10s `AbortSignal` covers the body, not just the headers. Neither is a flag you can pass |
| Read `/ask/stream` with a browser `EventSource` | `fetch` + `features/chat/sse.ts` | `event: done` carries **no `data:` line** — `Event::data("")` writes nothing — and the WHATWG dispatch step drops a data-less event. `EventSource` would therefore never fire `done`, silently, and every completed answer would look truncated. Our decoder departs from the spec on exactly this point, and says so |
| Join a frame's `data:` lines with `''` | Join with `'\n'` | axum re-prefixes every newline inside a value with a fresh `data: `, so one token arrives as several lines. Joining without the `\n` collapses a numbered list into one line — the answer is still *plausible*, which is why nothing catches it |
| Size `max_tokens` for the answer you want | Size it for the answer **plus the thinking you cannot see** | A reasoning model bills `reasoning_content` against the same budget, and `Delta` only reads `content`. Spend the budget on thinking and the completion is *empty*: `finish_reason: "length"`, zero content deltas, no error, `done` yielded normally. The bot answers nothing and nothing anywhere says why. 512 left ~80–180 tokens of margin — enough to look fine until a longer document or a cross-lingual question ate it |
| Trust an empty answer to look like a failure | `ask.ts` reports zero tokens as one | Retrieval succeeding and the model saying nothing is a *success* everywhere in the stack — the client is the only place that can notice the silence. A refusal is the opposite and must stay `ok` (invariant 4) |
| Put an error's own text in an SSE `error` frame | A fixed string; `tracing::error!` the detail | The frame goes to a browser — for a `pk_` widget, a *stranger's*. `{e:#}` from `llm.rs` is `LLM replied {status}: {the gateway's raw body}` — a body we do not author and cannot predict. Observed from a 401: a key fragment, **the key's full SHA-256 hash**, and the gateway's internal table names. A mid-stream frame never passes through `AppError::into_response`, so it is the one client-facing surface that does *not* inherit invariant 16 from `?` — it must re-implement that discipline by hand |
| `.timeout()` a client whose response body is a **stream** | `read_timeout` for the stall; a per-request total only on the non-streaming calls | reqwest's client `timeout` is a *total deadline including the body*, and `answer_stream` **is** a body. It therefore caps answer length, not gateway hangs — and the truncated turn is then dropped from history by invariant 7, so the user loses the answer *and* the record of asking. Exactly the `client.ts`-vs-`stream.ts` trap two rows down, one layer lower and in Rust (invariant 28) |
| Reuse `ASK_TIMEOUT_MS` for the API's gateway bounds | Named constants in `llm.rs` / `embedding.rs` | It is a **`web/` env var, in milliseconds**, bounding the BFF's own browser — `Duration::from_secs` on it is 33 hours. The API's client is the `pk_` widget, which has no BFF in front of it. Different bound, different client, different file |
| Put one timeout on `EmbeddingClient` and call it done | Size it for a full `EMBED_BATCH`, not for a question | It is shared by the **api and the worker**. A bound tight enough for a one-input question embed fails a 96-input ingest, and a failed ingest is `Transport` → retryable → five redeliveries → dead-lettered. The tight bound belongs to `read_timeout`, which fires on silence regardless of batch size |
| Point an integration test at `bot_flow` because it is already in `.env` | `bot_flow_test`; the harness refuses any name not ending `_test` | The suite creates and deletes tenants and documents. Sharing the dev database is one truncation away from a bad afternoon, and nothing would warn you first |
| Connect the harness as `bot_flow` because it is the URL that works | `app_user` — and the harness asserts `NOT rolsuper` | Superusers bypass RLS **entirely**, so every isolation assertion passes without testing anything. Green, meaningless, permanently reassuring. This is the single reason the guard exists |
| Assert only that tenant B sees nothing | Also assert tenant A still does | A broken embedding stub, a mis-set score floor, or a missing tenant context makes *everyone* see nothing — and the denial assertion passes on it. Verified: swapping `tenant_tx` for the plain pool in `list_documents` is caught **only** by the control, because RLS then denies A too |
| Drop the `lapin::Connection` returned by `build_state` | Bind it (`let (state, _amqp) = ..`) for the state's whole life | It closes the `Channel` inside `AppState`, and the only symptom anywhere is `/health` reporting rabbitmq down. `build_state` returns it rather than hiding it in `main` so the compiler carries half of this |
| Let a test skip itself when its service is missing | Fail, or do not run it at all | A silent skip turns "untested" into "green" — the same lie as the superuser trap, and worse than a visible gap |
| Rely on a test's own `cleanup()` to keep the Qdrant collection clean | Also sweep stale test tenants at startup | `cleanup()` runs only when a test **passes**. A panicking test — i.e. every failing one, and every break-verification — strands its points forever, because `/ingest` writes them with random ids and no `document_id` and nothing in the product can remove them. Observed: this suite's own break table leaked four tenants |
| Judge a chunking recipe on recall alone | Read context cost beside it | "Did a passage contain the answer" is trivially satisfied by returning the whole document. Measured: one-chunk-per-document scored a *perfect* recall and a *better* MRR while handing the model 1.8× the context. On recall alone the deliberately-broken variant wins |
| Pick `RAG_SCORE_THRESHOLD` by reasoning about it | Sweep it on the bench | Every value in this repo's history was wrong: 0.70 (E5-era) refuses everything, 0.35 silently drops 4.5% of answers. 0.25 is the highest floor that costs no recall — and it was only knowable by measuring |
| Change the chunker, the model or the payload in place | Bump `common::COLLECTION` | `ensure_collection` early-returns when the collection exists, so an in-place change **silently does not happen**. The version is also this system's only rollback: the old collection stays queryable while the new one fills |
| Trust `sqlx::migrate!` to notice a new migration file | Rebuild the crate that embeds it (touch a source file, or `cargo clean -p worker`) | It is a **compile-time** read of a directory, exactly like `include_str!` for `widget.js`. The worker embeds `../api/migrations`; adding `0013` and running `cargo test --workspace` failed with `VersionMissing(13)` because the worker binary still held the old set. The database was ahead of the binary, and the error names the version rather than the cause |
| `ORDER BY created_at` when the SELECT has `created_at::text AS created_at` | Qualify it: `ORDER BY documents.created_at` | Postgres resolves a **bare** name in ORDER BY against the *output* list first, so the alias wins and the sort runs on the text rendering. Measured on 5k rows: `Seq Scan` + `Sort Key: ((created_at)::text)` at cost 371, versus an index scan with **no sort node** at cost 0.29 once qualified. The 0016 index exists for this one query and the bare form never touches it. It reads like a correctness bug too — the keyset `WHERE` compares `timestamptz` — but is not: `timestamptz` normalises to UTC and renders in one session zone, so text order agrees with chronological order. The bug is the plan, not the result |
| Paginate a polled list with `LIMIT/OFFSET` | Keyset on `(created_at, id)` | A row inserted between two polls shifts every later row by one, so the reader sees a row twice or skips one — and neither is an error. The `id` half is not optional either: `created_at` defaults to `transaction_timestamp()`, so same-transaction rows are byte-identical, and a cursor without a tiebreaker loses exactly the rows on a boundary. Verified by deleting the tiebreaker: **5 of 9 documents vanished** and the listing still looked fine |
| Build the cursor query string by concatenation | `URLSearchParams` / `encodeURIComponent` | The cursor carries `+00`, and a bare `+` in a query string decodes to a **space** — corrupting the offset, not merely the separator. It fails only at a page boundary, never on page one, which is where anyone would look |
| Ship a nav item, plan or menu entry for a page that does not exist | Delete it, or build the page | A tenant who clicks Billing and finds nothing learns the UI does not mean what it says — and then every *true* claim has to earn belief separately. The sidebar carried Design Engineering / Sales & Marketing / Travel / Team / Billing for phases; none of it lost data, which is exactly why it survived |
| Link every prefix when deriving a breadcrumb from the URL | Only paths in `LINKABLE` | `/settings/password` exists, `/settings` does not — so the obvious loop puts a guaranteed 404 inside the one component whose whole job is orientation |
| Render a paginated count as a total | `formatCount(n, partial)` → `200+` | `GET /documents` returns no `total` on purpose (a count is the full scan pagination replaced), so a bare `200` for a 5,000-document tenant is a number that quietly means "the first page" — plausible, and wrong |
| Count all API keys to decide the widget is set up | Count `kind === 'publishable'` only | Every tenant gets an `sk_` at registration, so counting all keys marks the step done for everyone on day one, forever — and an `sk_` cannot drive the widget |
| Deploy `web/` without `ORIGIN` | Set it, or set both `PROTOCOL_HEADER` and `HOST_HEADER` | adapter-node infers the origin from the connection, so behind TLS termination the app sees `http://` while the browser sends `https://`. That mismatch fails SvelteKit's CSRF check **and** both hand-rolled `Origin` guards — every form post, upload and playground question 403s, with nothing in any log naming the cause. `env.ts` now refuses to boot without it |
| Read a required env var lazily and call it validated | Check it at startup (`assertRuntimeEnv`) | A lazily-read `API_BASE_URL` let the process **start, bind, pass a TCP healthcheck, and then 500 every page**. An orchestrator calls that healthy. A process that refuses to start is a deploy that visibly fails |
| Set `ASK_TIMEOUT_MS` below the API's `STREAM_DEADLINE` | Keep it strictly above (330s vs 300s) | The API's ceiling is *graceful* — it ends the stream with `done` and persists what arrived. The BFF's just aborts the fetch. If the BFF fires first the user loses the answer **and** the turn, which is the exact outcome `STREAM_DEADLINE` was designed to avoid. Two codebases, no compiler between them |
| Assume `web/build/` needs `node_modules` at runtime | It does not | `adapter-node` emits a self-contained bundle — verified by running `node build/index.js` in an empty directory with no `node_modules` at all. The runtime image therefore copies only `build/`, and carries no package manager or dependency tree |
| Return `404`/`422` from `/auth/password/forgot` for an unknown or malformed address | Always `202` | It is the same oracle `/auth/login` refuses to be, rebuilt on a new endpoint. Validating the *shape* leaks too: a 422 for a malformed address tells an attacker which shapes are accepted, and the two paths then differ. Garbage simply matches no row |
| `await` the reset email before responding | Spawn it | A known address would then cost an SMTP round trip and an unknown one nothing, making the response *time* the oracle the status code is not. Measured after spawning: 2.3–3.3ms vs 1.5–2.5ms, overlapping |
| Log the reset link "just while debugging" | Log that a send failed, and to whom | The link **is** the credential, and redeeming one takes the account outright. A link in a log file is invariant 14 broken by the newest secret in the system |
| Issue a session when a reset succeeds | Redirect to login | Redeeming a link proves control of an inbox, not knowledge of the password. Handing back a session means a leaked link becomes a live login without the password ever being typed |
| Return `401` when the current password is wrong on change-password | `403` | The BFF clears its session cookie on a 401 (invariant 21), so a 401 signs the user out for a typo — on the one page where they are proving they are still themselves |
| Give a reset token the `sess_` prefix, or no prefix | `rst_` | `Actor` dispatches on `sess_`. A reset token wearing it would be resolved against `sessions` — offering a password-*changing* credential to the path that grants access |
| Map an unrecognised failure to the tenant's fault | `system_error` unless the sidecar *proves* otherwise (exit 3 or 4) | The two errors are not symmetric. Excusing a broken file wastes one support ticket; blaming a tenant for our outage sends them re-uploading a good file in a loop that cannot succeed, while the real fault goes unreported. `classify` therefore has one narrow `match` and a catch-all, and `status.ts`'s `system_error` branch is the one place the word "upload" must never appear — pinned by a test |
| Add a `failure_reason` variant in one place | Three places, and only one fails closed | `failure.rs`, migration 0015's `CHECK`, and the TS union. A worker writing a variant the `CHECK` lacks aborts its transaction — loud, and the good case. A variant the **TS** lacks renders as the cause-agnostic copy — silent, and merely vague. Adding to TS alone is the dangerous order: the UI promises copy for a value nothing writes |
| Trust `documents.error` to stay unexposed because it always has been | Select `failure_reason`, never `error` | They sit one column apart in the same row and the same `SELECT`. `error` holds the gateway bodies and stack traces invariant 16 exists to contain, and adding it "just for debugging" is a one-word diff that reviews clean |
| Label a metric with `tenant` | Aggregate only; `GET /admin/ops/tenants` for attribution | Invariant 30. A TSDB is a fourth store the erasure saga cannot reach, is designed not to be deleted from, and its series outlive the tenant. Hashing the id does not help — a stable pseudonym survives erasure too |
| Scrape `/metrics` with `ADMIN_API_KEY` | A separate `METRICS_TOKEN`; no token means no route | The admin key can **erase a tenant** (phase 12), and a scrape config is a low-trust, widely-readable artifact. Unset means the route is not registered at all — an endpoint that refuses still confirms it exists |
| Read gauges with `SELECT count(*) FROM documents` on the app pool | `metrics_document_counts()` (`SECURITY DEFINER`, aggregates only) | `documents` is RLS-FORCED and the API is `app_user`: it matches zero rows and *reports success*. Every gauge reads 0 and the dashboard is permanently, beautifully green — in the one endpoint whose job is to say something is wrong |
| Add a `tenant_id` column to `metrics_document_counts()` | Keep the return type identity-free | It bypasses RLS, and the absence of identity in its **signature** is the only thing that makes that safe. Adding a column turns a metrics helper into a cross-tenant read path, and every caller still looks correct |
| Passive-declare a queue on `state.amqp` | Open a throwaway channel from `amqp_conn` | A `NOT_FOUND` closes the channel, and the only symptom is `/health` reporting rabbitmq down — the `lapin::Connection` trap one layer up. One scrape of a fresh deployment would break publishing |
| Report `0` for a queue you could not read | Emit no series at all | A `0` that means "I could not ask" is indistinguishable from a dead worker, and pages someone on a broker hiccup. `absent()` handles the missing case |
| Add a latency histogram to `metrics.rs` | Take the `metrics` crate instead | Hand-rolled `_bucket`/`le` semantics is where 30 lines becomes 150, and where being subtly wrong yields plausible numbers — this system's characteristic failure mode |
| Back up MinIO before Postgres | Postgres first, always | Reverse it and a document uploaded mid-backup restores as a **row with no object** — a `ready` document whose bytes are gone, which fails `worker reindex`. The stated order yields a harmless orphan object instead |
| `pg_dump` and call the backup complete | `pg_dumpall --roles-only` beside it | Roles are cluster-level. Without `app_user`, restoring into a fresh cluster fails on the RLS grants that name it |
| `docker cp` MinIO's `/data` | `mc mirror` the objects via the S3 API | `/data` is the erasure-coded backend (`xl.meta`), which round-trips only into a byte-identical MinIO and is useless against real S3. Also: the first version of `backup.sh` did this with a `\|\| echo` fallback and produced a backup containing **zero objects while reporting success** |
| Count a backup's contents from the store's inventory | Count what was actually written | "What the store says it holds" and "what this archive contains" are different numbers, and only the second can be restored. The manifest reported `objects: 1` on an empty archive |
| Leave the api running through a restore | Restart it | `ensure_collection` runs at **startup**, so a running process never notices its collection was dropped, and the reindex then fails with "Collection doesn't exist" |
| Give an audit table the foreign key its neighbours have | `erasures` references nothing | It would cascade away in the same statement as the erasure it documents. Postgres will even refuse to add the constraint once real rows exist — the schema disagreeing with itself *is* the property |
| Query an RLS table from a test on the app pool | Set `app.current_tenant` first | `documents`, `conversations` and `messages` are RLS-forced. A direct read returns zero rows and *reports success* — the corollary trap, arriving in a test and reading as a missing feature. `TestApp::count_as_tenant` exists for this |
| Add a second way to write vectors | Write the bytes and let the worker index them | Invariant 29 breaks exactly one way: a path that skips the `documents` row. `/ingest` was that path for eight phases, and its points were unerasable the whole time. If a new route needs to index text, it should produce an object, not a point |
| Republish ingest events to re-index everything | `cargo run -p worker -- reindex` | Invariant 10 skips a redelivered document whose fingerprint is unchanged — the very thing that makes redelivery safe makes a migration a silent no-op. The driver bypasses `claim` deliberately, which is also why the normal worker must be stopped first |
| List only the tables you name in a `TRUNCATE … CASCADE` prompt | Name what CASCADE will reach as well | `accounts` and `sessions` hang off `tenants` by FK, so a wipe of "tenants, api_keys, documents" silently destroys every dashboard login too. Verified from the `NOTICE` output. A prompt that understates its blast radius is approved by someone who did not know what they were approving |

The sidecar signals failure with **exit codes, not stderr**, and the code says *whose fault it is*:

| Code | Meaning | Whose fault | `failure_reason` |
| --- | --- | --- | --- |
| `0` | text on stdout | — | — |
| `1` | uncaught crash (a missing `pypdf`, a bug) | **ours** | `system_error` |
| `2` | usage error — wrong argv count | **ours** (the worker always passes one arg) | `system_error` |
| `3` | unsupported extension | the document's | `unsupported_type` |
| `4` | unreadable content: encrypted, truncated, malformed | the document's | `unreadable_file` |

This table was wrong in this file for eleven phases — it read *"`2` = unreadable, `3` = unsupported
type"*. `2` is argv misuse, which `parser.rs` cannot trigger; a genuinely unreadable PDF was an
**uncaught traceback on `1`**, indistinguishable from our own sidecar being broken. `4` exists
because that distinction is now load-bearing: it decides whether a tenant is told to re-upload or to
wait, and mapping `1` to "your file is damaged" would tell someone to re-upload a good file every
time *our* deployment broke. Hence the rule inside `parser.py`: **when in doubt, crash (1)** — never
widen `4` to cover an internal fault.

It is a cross-language contract and neither side documents the other — change one, change both.
`failure::classify` is the Rust half, and it is *conservative by construction*: only codes 3 and 4
implicate the document, everything else is `system_error`. The worker separately classifies
fatal-vs-retryable for the **queue**, which is a different axis and must not be conflated — a
`Retryable` failure still writes a `failure_reason`, because the tenant sees the row now and the
redelivery may be twenty minutes away. An unclassified error in the worker is a bug: if you cannot
say whether a failure is fatal or retryable, that is the design question to answer before writing
the code.

## Security

- **Hash, don't encrypt.** Keys carry ~244 bits of entropy (two v4 UUIDs) and only their SHA-256 hex
  reaches the database. An encrypted key can be decrypted by whoever holds the key-encryption key,
  which is on the same machine; a hash cannot be reversed, so a database dump is not a credential
  dump. The cost — we can never show a key again — is the intended trade.
- **A gate is the first line of any handler that ingests, uploads, lists or manages** — which gate
  depends on who may legitimately reach it:
  - `actor.require_management()?` — `sk_` **or** `sess_`, never `pk_`. On `GET /documents`,
    `DELETE /documents/{id}`, both upload-url routes, and `/search`: the tenant's own server *and* the
    dashboard reach these (invariant 23). `/search` belongs here because raw retrieval is not "asking
    a question" — invariant 15's first sentence is this gate, not a description of intent.
  - `tenant.require_secret()?` — `sk_` only. On `/ingest` and the deprecated multipart
    `POST /documents`. Both are paths we are not extending, so neither gets a session.

  Correctly absent on `/ask` and `/ask/stream`, which take `Actor` and gate nothing: all three kinds
  pass, because `pk_` is *meant* to reach them and the other two are strictly stronger (invariant 27).
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
| Composition root — `app()` (routes + CORS), `build_state()` (every dependency; returns the amqp `Connection` so it cannot be dropped), `run_migrations()`. `main.rs` is a ~25-line shell over it | `crates/api/src/lib.rs` |
| The integration harness — test-database bring-up, the two guards (`_test` name, `NOT rolsuper`), the `TestApp` fixture and its Qdrant teardown | `crates/api/tests/common/mod.rs` |
| The fake LLM/embedding gateway, and the deterministic content-addressed embedder that makes a denial assertion unambiguous | `crates/api/tests/common/gateway.rs` |
| Cross-tenant denial (both mechanisms, each with its control), the auth matrix, and the deletion saga's API half | `crates/api/tests/{tenant_isolation,auth_matrix,deletion}.rs` |
| Worker integration tests — concurrent claim and the fence (`lifecycle.rs`), the lease-respecting sweeps (`reaper.rs`), and their shared setup. In-crate on purpose; the module doc says why | `crates/worker/src/{lifecycle,reaper,testsupport}.rs` |
| CI — and why `cargo test` runs *before* the services start, and why `bun run lint` is absent | `.github/workflows/ci.yml` |
| Handlers, Qdrant search, SSE stream | `crates/api/src/handlers.rs` |
| The keyset cursor — `encode_cursor` / `parse_cursor` / `is_our_timestamp`, and why a cursor we did not mint is a 422 rather than a 500 | `crates/api/src/handlers.rs` |
| `STREAM_DEADLINE` — the wall clock on a streamed answer, and why firing it is a `done` rather than an `error` | `crates/api/src/handlers.rs` |
| That paging loses no row, including across identical timestamps | `crates/api/tests/pagination.rs` |
| The `/ask/stream` frame contract and the turn it persists — the route's first tests | `crates/api/tests/ask_stream.rs` |
| Page navigation as links, so reading the library still needs no JavaScript | `web/src/lib/features/documents/components/pager.svelte` |
| `tenant_tx()` and the two pools | `crates/api/src/db.rs` |
| `AuthTenant` / `AdminAuth` / `SessionAuth` / `Actor`, `hash_key`, `require_secret` vs `require_management`, the `sess_` prefix dispatch, `normalize_origin` (next to the `Origin` check it must agree with), key + session token gen | `crates/api/src/auth.rs` |
| Self-serve accounts: register / login / logout / me / self-serve key mgmt; Argon2 hashing | `crates/api/src/accounts.rs` |
| Password reset + change: the non-oracle `forgot`, the single-use `reset`, and why a change is 403-not-401 | `crates/api/src/accounts.rs` |
| The SMTP transport, and the two rules about it — never log a link, never surface a send failure | `crates/api/src/mail.rs` |
| That a reset revokes every session, cannot be replayed, and tells nobody which emails exist | `crates/api/tests/password_reset.rs` |
| Forgot / reset / change-password pages, and the copy that must not confirm an address exists | `web/src/routes/(auth)/forgot-password/`, `.../reset-password/`, `web/src/routes/(authenticated)/settings/password/` |
| `AppError` and the blanket `From` impl that makes `?` a 500 | `crates/api/src/error.rs` |
| Env vars and their defaults | `crates/api/src/config.rs` |
| Worker claim / status machine | `crates/worker/src/lifecycle.rs` |
| `FailureReason` and `classify` — the closed enum behind a `failed` badge, and why anything unrecognised is *our* fault | `crates/worker/src/failure.rs` |
| The sidecar exit-code contract in Rust (`SidecarExit`, `EXIT_UNREADABLE`) — the typed error `classify` downcasts to | `crates/worker/src/parser.rs` |
| MinIO event parsing (the percent-encoding trap) | `crates/worker/src/event.rs` |
| Chunking — `chunk_text` and the `CHUNK_SIZE`/`CHUNK_OVERLAP` constants. In `common` because it is half the index recipe: the worker writes chunks and the bench must reproduce them byte for byte | `crates/common/src/chunk.rs` |
| The retrieval bench — fixture corpus, golden set, the metrics, and the sabotage table that must move before any number is believed. Writes to `eval_bench`, never the live collection | `crates/eval/` |
| The lexical (sparse) encoder — written from phase 10, queried in 10b; FNV ids pinned by test because a changed hash orphans every dimension already written | `crates/common/src/sparse.rs` |
| The versioned collection name, and why a version is the only rollback this system has | `crates/common/src/lib.rs` |
| The migration driver (`worker reindex`) — why it bypasses `claim`, and why the worker must be stopped | `crates/worker/src/main.rs` |
| `worker purge-unattributed` — erasing pre-phase-11 points that belong to no document; dry-run by default, optionally scoped to one tenant | `crates/worker/src/main.rs` |
| Inline documents: `checked_extension` (shared with the presigned path) and `inline_document` (the row, and `external_id` reuse) | `crates/api/src/upload/mod.rs` |
| That an ingested document is listable, deletable, and gone afterwards | `crates/api/tests/ingest_erasure.rs` |
| Erasing a tenant across all three stores, the audit trail, and redacting answers that quoted a deleted document | `crates/api/src/erasure.rs` |
| That a tenant's data goes and the record of it staying does not | `crates/api/tests/tenant_erasure.rs` |
| Reaper — `UPLOAD_GRACE`/`PROCESSING_LEASE` constants; expired/reclaimed sweeps **and** the deferred-deletion sweep (phase 8) | `crates/worker/src/reaper.rs` |
| `DELETE /documents/{id}` — the tombstone-guarded saga and `delete_document_stores` (its order/filters mirror the reaper sweep) | `crates/api/src/handlers.rs` |
| PDF/text extraction, exit codes 2 and 3 | `sidecar/parser.py` |
| Embeddable widget — renders citations, counts token frames, handles `done` | `widget/widget.js` |
| The route that serves the widget from the binary (`include_str!`), with the ETag/`no-cache` revalidation that lets a fix reach every visitor | `crates/api/src/widget.rs` |
| The SSE frame contract, and its only *tested* parser — `widget.js` has a second, untested one, now *deployed by us* rather than copied by tenants (D4 kept it separate; the stakes rose) | `web/src/lib/features/chat/sse.ts` |
| Browser-side ask: the relative-path/no-credential boundary, and where invariant 4's "a refusal is an answer" is decided by structure rather than by matching `NO_ANSWER` | `web/src/lib/features/chat/ask.ts` |
| Citations in the UI — the only surface that renders them, and why `sources[].index` is never renumbered | `web/src/lib/features/chat/sources.ts` |
| Web BFF hinge — session cookie → `GET /auth/me` → `locals` | `web/src/hooks.server.ts` |
| Startup config validation — why `ORIGIN` is refused-at-boot rather than discovered from a 403 | `web/src/lib/server/env.ts` (`assertRuntimeEnv`) |
| The web image: bun builds, node runs, and only `build/` ships | `web/Dockerfile` |
| The go-live checklist, and why it counts only publishable keys and never states a total | `web/src/lib/features/dashboard/readiness.ts` |
| Route → breadcrumb, and the prefix that must not be linked | `web/src/lib/utils/breadcrumb.ts` |
| Typed API client: `ApiResult`, the JSON-vs-`text/plain` split, timeouts | `web/src/lib/server/api/` |
| The SSE proxy: why the JSON client cannot carry a stream, and the ceiling that replaces its 10s | `web/src/lib/server/api/stream.ts` |
| Session + one-time-key cookies; `requireUser` vs `requireSession` | `web/src/lib/server/auth/` |
| Login-401 and the two register-409s → which field (invariant 19) | `web/src/lib/features/auth/error-map.ts` |
| The TS mirrors of the Rust validators — drift here 422s/400s the user | `web/src/lib/features/{auth,documents}/schema.ts` |
| Browser → MinIO upload; the presigned-URL-is-not-a-credential boundary (invariant 24) | `web/src/lib/features/documents/upload.ts` |
| Origin validation + the mint/PATCH rules, shared by admin and self-serve | `crates/api/src/handlers.rs` (`checked_origins`, in `insert_api_key`) |
| The embed snippet, and its refusal to carry an `sk_` | `web/src/lib/features/keys/embed.ts` |
| Status (+ `failure_reason`) → user-facing copy; where invariant 16 is enforced *in the UI*, and the only place the re-upload-vs-wait decision is made | `web/src/lib/features/documents/status.ts` |
| The two BFF routes a browser fetches directly — the shared origin / content-type / `locals.session` guard chain, and why it is not `requireUser` | `web/src/routes/(authenticated)/documents/upload-url/+server.ts` (mint + re-mint) and `.../playground/ask/+server.ts` (the SSE proxy) |
| Migrations — forward-only, run at API startup on the admin pool, which is then closed | `crates/api/migrations/` |
| Counters, the exposition format, and why there is no metrics dependency (plus the stated stopping point: no histograms) | `crates/api/src/metrics.rs` |
| The `SECURITY DEFINER` gauge functions, and why bypassing RLS is safe only because their return type has no identity | `crates/api/migrations/0014_metrics_functions.sql` |
| Backup/restore, what is deliberately not backed up, and the order that is a correctness decision | `scripts/backup.sh`, `scripts/restore.sh` |
| Alert rules, and an explicit note on which of them have ever been fired | `doc/ops/alerts.yml` |
| Clean-slate wipe of all five stores, plus `bot_flow_test` and `eval_bench`. Names `accounts`/`sessions` in its prompt because `CASCADE` reaches them either way | `scripts/reset.sh` |

## Known state & debt

Honest inventory. Each entry states the impact, not merely the fact.
**[`doc/production-readiness.md`](doc/production-readiness.md) is the narrower filter over this list**:
it asks only what stops real customers using the system, and orders by that. This section is the
superset — everything known, whether or not it blocks.

- **`POST /ingest` is a supported path now, and the debt it carried is closed (phase 11).** It used
  to write vectors with random ids and no `document_id`: unlistable, un-re-indexable, unremovable —
  *"permanent"*, and the largest single piece of debt in the system. It now writes the caller's text
  to MinIO as an ordinary object and lets the worker index it, so it inherits the document row, the
  lifecycle, chunking with full provenance, the deletion saga, the reaper and the re-index driver.
  The contract broke to get there: `{"texts": [...]}` became `{"filename", "text", "external_id?"}`
  and it answers `202`, not `200` — indexing is asynchronous now, because the worker does it.
  Two residues worth knowing. **Vectors written by the old path are still unattributed**, and no
  migration can fix that (nothing ever recorded which ingest produced which point) —
  `cargo run -p worker -- purge-unattributed [tenant] [--yes]` erases them, dry-run by default,
  because it is someone's working corpus. And the playground's *"no documents are ready"* warning is
  correct for every path again, since an `/ingest` document is now a document.
- **The playground cannot reproduce the most likely go-live failure.** It authenticates with a
  `sess_`; the tenant's real widget uses a `pk_` bound to an `Origin`. So an `allowed_origins`
  mismatch — invariant 15's "403s forever with nothing in any log to say why" — is invisible here: the
  playground answers happily while the widget is dead. The page says so and links to `/keys`, which is
  the honest mitigation, not a fix. A preview that hides the most common production failure is worse
  than no preview if it does not admit it.
- **Playground traffic shares the tenant's rate-limit bucket with their live widget**, because
  `rate_limit::check` keys on `tenant_id` rather than on the credential. That is what bounds the spend
  (invariant 27), and the cost is that a tenant testing enthusiastically can throttle production.
- **A deferred deletion can still answer for up to one `PROCESSING_LEASE`.** `DELETE /documents/{id}`
  erases a document across all three stores (phase 8): it tombstones the row to `deleting`, then
  removes vectors → object → row, the order that fails toward the least-bad orphan. For a document no
  worker is touching this is synchronous (`204`) and instant. But a delete that lands *while a worker
  is indexing* returns `202` and defers the store-cleanup to the reaper sweep, which cannot safely
  delete the vectors until the worker has provably released — i.e. `PROCESSING_LEASE` after indexing
  began. Until then the row is gone from the tenant's listing but its vectors still answer searches.
  It affects only a delete racing an active index (rare), and the bound is the lease, but for an
  *erasure* feature a ~30-minute "deleted but still answering" window is a real gap. Closing it means
  the worker signalling completion on a `deleting` row so the sweep need not wait out the lease —
  deferred, not free, because that write must not become a way to resurrect the tombstone (invariant
  10). The synchronous path — the overwhelming majority of deletes — has no such window.
  **Phase 9b pins this window rather than closing it**, in both directions: the sweep must *not*
  erase a row whose lease is still live (doing so would race the worker's in-flight upsert and
  strand orphaned vectors) and must erase one whose lease has elapsed. So the trade is now a tested
  decision instead of an assumed one — and whoever closes it has a test that will tell them if the
  fix reintroduces the race.
- **`GET /documents` is paginated (phase 15).** It used to return the tenant's entire table, every
  call, fully materialised, on a route the dashboard polls. It now takes `?limit=` (default 50, max
  200) and `?before=<cursor>`, and answers `{documents, next_cursor, limit}`.
  **Keyset, not `OFFSET`, because the list is polled**: with an offset a document created between
  two polls shifts every later row by one, so the reader silently sees a row twice or misses one.
  The cursor is `(created_at, id)` and **both halves are required** — `created_at` defaults to
  `now()`, which is `transaction_timestamp()`, so rows written in one transaction are byte-identical
  and a cursor on the timestamp alone loses exactly the rows on a page boundary. Migration 0016 adds
  the `(tenant_id, created_at desc, id desc)` index the sort never had.
  Three residues. The **default is the load-bearing part** — it is what bounds an un-updated client,
  which is why `limit` is optional rather than required. There is **no `total`**, deliberately: a
  count is the full scan this replaced. And **the cursor cannot go backwards**; the dashboard uses
  browser history for that, which is free and correct but means "previous page" is unavailable to an
  API client that did not keep its own cursors.
- **`failed` no longer conflates "your file is broken" with "our worker died" (phase 14).** It used
  to: `mark_failed` wrote the parser's stderr and the reaper wrote `'processing lease expired; worker
  presumed dead'`, both into the `error` column that **no endpoint exposes** — correctly, since
  invariant 16 forbids shipping either string to a client. So the UI had to name both causes at once
  and a tenant could not tell whether to re-upload or wait.
  The worker now writes a *classified* `failure_reason` beside the raw text — a closed enum
  (`unreadable_file` / `unsupported_type` / `too_large` / `system_error`) cut by **what the tenant
  should do**, not by what broke. `GET /documents` returns the code; `error` stays unexposed.
  Three residues worth knowing. **Rows that failed before this exist with `failure_reason IS NULL`**
  and are not backfilled — nothing recorded a cause for them, and grepping the old `error` text for
  the reaper's string would be inventing a fact from free text that was never a contract; the UI
  renders null as the old both-causes copy. **The enum lives in three places** (`failure.rs`,
  migration 0015's `CHECK`, `web/src/lib/types/documents.ts`) and only the `CHECK` fails closed.
  And **`error` is still not classified for operators** — the reason code is deliberately coarse, so
  diagnosing *which* store was down still means reading logs.
- **`POST /documents` (multipart proxy) is deprecated** — it buffers whole files in API memory. Use
  `POST /documents/upload-url`. It gets deleted along with `crates/api/src/queue.rs` and the worker's
  `consume_legacy` / `LEGACY_QUEUE`. Do not add features to it. Do not add callers.
- **`POST /auth/keys` is metered (phase 15).** A logged-in session could mint unbounded keys — not a
  spend problem (minting buys no embeddings) but an **audit and revocation-surface** one: every row
  is a live credential someone must later enumerate and revoke. It was the last route that created
  state without a meter.
  It takes its **own bucket**, `keys:{tenant_id}`, and the prefix is the point rather than tidiness.
  `rate_limit::check` keys on whatever string it is handed, so passing the bare `tenant_id` would put
  key-minting in the *same* window as `/ask` — a tenant provisioning keys would spend their own
  question budget, and their widget would start 429ing for a reason no log connects to the dashboard
  tab that caused it. Verified: at `RATE_LIMIT_PER_MINUTE=5`, mints 1–5 returned 201, 6–7 returned
  429, and `POST /documents/upload-url` still returned 201 on the same session.
  `GET`/`PATCH`/`DELETE` on `/auth/keys` stay unmetered: they create no rows, and their cost is
  bounded by the number of keys that already exist.
- **The isolation guarantee is tested now — here is exactly how far that goes.** Phase 9 added
  `crates/api/tests/` and `.github/workflows/ci.yml`. **Covered:** cross-tenant denial on both
  mechanisms (RLS row reads/deletes, *and* the Qdrant tenant filter, each with the control assertion
  that the victim tenant still sees its own data); the full auth matrix — `pk_` refused by
  `require_management()`, `pk_` admitted to `/ask`, origin rejection including an absent `Origin`,
  `sess_` where invariant 23 says it belongs, and 401-vs-403 kept distinct. Each was verified to go
  **red** against a deliberate break before being trusted.
  Phase 9b added the worker half: **concurrent claim** (two workers racing one row — invariant 10's
  whole deduplication story, looped ten times, and red at round 1 without `FOR UPDATE`); the
  **phase-8 fence** (`mark_ready`/`mark_failed` cannot resurrect a `deleting` row); the **deletion
  saga** on both sides — the API's `204`-vs-`202` split and tombstone, and the reaper sweep's
  refusal to erase a row whose lease is still live; **lease reclaim**; and two worker-side tenancy
  assertions (a foreign document is invisible to `claim`; a sweep bound to one tenant cannot touch
  another's rows, which is the corollary trap asserted rather than trusted).
  Phase 15 added the pagination suite (`pagination.rs` — the tie case, the guard rails, and the
  bounded default) and, for the first time, **`/ask/stream`** (`ask_stream.rs` — frame order, the
  data-less `done`, the persisted turn, a refusal costing no LLM call, and a `pk_` reaching the
  route). Both were verified red first: deleting the cursor's `id` tiebreaker loses 5 of 9 rows.
  **Not covered, and worth knowing precisely:** `STREAM_DEADLINE` *firing* — at 300s a real test
  would take five minutes and a faked clock would assert against a stub, so the bound is reasoned
  and reviewed rather than proven. Also the *migration* driver, which is
  exercised by hand rather than by a test. Retrieval quality is no longer uncovered but it is
  measured rather than asserted — see the bench. And one gap that is structural rather than deferred — removing the
  `tenant_id` leg of `delete_document_stores`' Qdrant filter turns **nothing** red, because
  `document_id` is a globally-unique UUID and no test can construct a collision. That filter is
  layered defence, and the suite cannot prove it; a test that *could* would be asserting on internals.
  CI is **report-only** — no branch protection yet.
- **Vector storage now has a migration path — used once, and still expensive.** Phase 10 gave it the
  two things it lacked: a **versioned collection** (`common::COLLECTION`, now `documents_v2`) so the
  old one stays intact and queryable while the new one fills, and a **driver**
  (`cargo run -p worker -- reindex`) that re-embeds every document of every tenant from its stored
  object. Cutover and back-out are both one constant. What has *not* changed is the cost: it is still
  a full re-embed of every chunk, still billed, and still something to schedule rather than trigger.
  Two hazards remain, both by construction. **`/ingest` chunks cannot be migrated at all** — no
  `document_id`, no source object, so a re-index abandons them where they are. And the driver holds
  no claim, so **the normal worker must be stopped** or the two can interleave on one document.
  Historically: changing the embedding model, its dimension, or the chunking parameters invalidates
  every stored vector, and there was no rollback — only recreating the collection and re-indexing
  every document of every tenant. A **partially** re-indexed collection
  produces quietly degraded retrieval with no error anywhere. Any such change is a migration project,
  not a configuration change.
  This was paid, not solved, at the `MultilingualE5Small` → `text-embedding-3-small` cutover
  (384 → 1536 dim): all vectors and all `documents` rows were truncated and re-uploaded. Note the
  second half of that — `ensure_collection()` **early-returns when the collection exists** and will
  not rebuild it, and invariant 10 *skips* a redelivered document whose fingerprint is unchanged. So
  dropping the collection alone leaves it permanently empty, with no error. The document rows must go
  too. Whoever changes the model next will hit both.
- **`/ask/stream` has a maximum duration now (phase 15), and invariant 28's residue is closed.**
  Every gateway call was bounded, but the streaming answer's bound was a *stall* — silence between
  reads — so a gateway trickling one token just inside `READ_TIMEOUT` streamed indefinitely while
  never once looking unhealthy. `STREAM_DEADLINE` (300s, `handlers.rs`) is an absolute wall clock,
  taken **once before the loop**: a per-token deadline would reset on every delta and bound nothing,
  which is the mistake that makes this look already-solved.
  **It lives in the handler's loop, never on the reqwest client.** A client `.timeout()` is a total
  deadline *including the body*, and this body is the answer — it would cap answer length rather
  than gateway hangs (`llm.rs` has two tests that go red if someone adds it).
  **Firing is not a failure, and that is the design.** The obvious version sets `failed = true`,
  which emits an `error` frame after the client has already rendered good prose *and* skips
  `append_turn`, so invariant 7 drops from history a turn only our own ceiling truncated — losing the
  answer and the record of asking. Instead it emits a normal `done` and **persists what arrived**.
  The cost, real and accepted: an answer cut mid-sentence becomes history the next rewrite reasons
  over. Still open: **the deadline firing has no test** — at 300s a real one would be a five-minute
  test and a faked clock would assert against a stub. The frame contract around it is now covered.
- The DB permits an `uploaded` document status that **no code path ever assigns**. A vestige. Either
  give it meaning or drop it from the constraint — an unreachable state is a trap for the next reader.
- **Account recovery exists (phase 16), and here is exactly how far it goes.** `POST
  /auth/password/forgot` mails a single-use, one-hour link; `POST /auth/password/reset` redeems it,
  revokes **every** session and burns the account's other outstanding links; `POST /auth/password`
  changes a password you still know, keeps the current session and revokes the others. Mail goes
  through `lettre` to `SMTP_URL` — **Mailpit in `docker-compose.yml` for development**, so a live
  reset link can never reach a real inbox from a dev box.
  **Three new required env vars**, and they fail at boot rather than at first use: `SMTP_URL`,
  `MAIL_FROM`, `APP_BASE_URL`. That is deliberately unlike `METRICS_TOKEN`'s "absent config, absent
  surface" — a missing metrics token costs a dashboard, a missing mailer costs a locked-out user
  their account, silently, because `forgot` answers `202` either way.
  **Still open, and worth knowing:** there is **no email verification**, so an address is still
  unproven at signup (`accounts.rs` has said so since phase 2) — which means recovery is only as
  good as the address someone typed. There is **no durable outbox**: the send is a spawned task, so
  an email lost to a crash between the `202` and delivery is lost silently and the user must ask
  again. Spent and expired token rows are **never swept** (harmless — both guards are in the
  redemption query — but the table only grows). And **delivery has no automated test**: the harness
  points `SMTP_URL` at a dead port on purpose, so the mail path is covered by a manual Mailpit drill
  recorded in the phase doc, while the suite covers redemption, revocation and the non-oracle.
- **The dashboard only claims what exists now (phase 18).** The sidebar advertised Billing, Team and
  three shadcn sample sections; the tenant switcher listed *Acme Inc* / *Evil Corp.* on fictional
  plans above the tenant's real name; the avatar was a photograph of a real person; the breadcrumb
  read *Build Your Application / Data Fetching* everywhere; and `/dashboard` — where onboarding lands
  every signup — was one line reading `dashboard tenant`.
  All of it is gone or real. `/dashboard` is now a **readiness checklist** (is a document indexed, is
  there a `pk_`, have you tried it) built from `GET /documents` and `GET /auth/keys`, `/` is a
  landing page whose every claim maps to documented behaviour, and there is a root `+error.svelte`.
  **Two things in it are load-bearing rather than cosmetic.** It counts only `publishable` keys —
  every tenant has an `sk_` from registration, so counting all of them would mark the step done for
  everyone forever. And it renders `200+` rather than `200` when there are more pages, because the
  API returns no `total` by design and a bare number would quietly mean "the first page".
  Residues: the dashboard's `load` is not tested, only its pure readiness logic — the first time the
  repo's "pure functions only" line has hidden something worth testing. And `web/` still sets **no
  security response headers**, so the `sk_` reveal page is framable by any origin.
- **`web/` is deployable now (phase 17), and `.env.example` exists.** The web app had no deployment
  path at all: the root `Dockerfile` built only the Rust binaries, compose had no `web` service, and
  `package.json` had no `start` script, so `bun run build` produced an artifact nothing ever ran.
  There is now `web/Dockerfile` (bun builds, **node** runs — `adapter-node`'s tested target), a
  `start` script, and an **opt-in** compose profile so `docker compose up -d` still starts only the
  backing services. `docker compose --profile full up -d --build web` brings the image up locally,
  which is what stops it rotting unnoticed.
  **The `ORIGIN` half mattered more than the packaging.** Behind TLS termination adapter-node infers
  `http://` while the browser sends `https://`, and every form post 403s — see the trap table.
  `assertRuntimeEnv` now refuses to boot without it, and checks `API_BASE_URL` and the numeric vars
  at startup rather than on the first request.
  Two residues. **`ASK_TIMEOUT_MS` was below the API's `STREAM_DEADLINE`** (120s vs 300s) so the BFF
  cut long answers before the API's graceful ceiling could — now 330s, but the ordering is a
  cross-codebase invariant with nothing enforcing it. And CI now runs `bun run build`, which it
  never did: the adapter lives in `vite.config.ts` (there is no `svelte.config.js`), so a
  build-breaking change used to ship green.

## Working here

- **Comments explain *why*, never *what*.** Record the trap or the trade-off, not a restatement of the
  line below it. Most of this file is comments that outgrew their source.
- **A behaviour change starts here.** Edit the invariant, then write the code — in the same commit.
  A document that quietly drifts from the code is worse than no document, because it is trusted.
- **Do not restate values this file does not own.** Chunk size and overlap, the relevance floor, the
  presign TTL, the upload cap, history depth, ports and the full `.env` block live in `README.md` and
  in the code. Duplicating them is how they drift. Point at the constant; don't copy it.
- **`cargo test` must pass with no backing services running** — inline `#[cfg(test)]` in Rust,
  `*.test.ts` beside the source in `web/`. That promise is intact and is now *enforced*: CI runs
  `cargo test --workspace` **before** it starts docker compose, deliberately, so a contributor
  without Docker is never punished. Move that step below the services and the guarantee silently
  stops being checked.
  The integration suite (phase 9) lives in `crates/api/tests/`, is `#[ignore]`d, and runs under
  `cargo test -- --ignored` with `docker compose up -d` plus `./scripts/test-setup.sh`. It needs all
  five services but **no LLM or embedding key**: both gateways are stubbed in-process.
  **A test that skips when its service is absent is forbidden.** Fail, or do not run at all. A silent
  skip turns "untested" into "green", which is the same lie as the superuser trap below — and worse
  than a visible gap, because it is permanently reassuring.
- **Where the seam is real API, split the lib; where it would exist only for the test, test
  in-crate.** One rule, applied twice, and the asymmetry is deliberate rather than untidy.
  `crates/api` has a `lib.rs` because `app` / `build_state` / `run_migrations` / `Config` /
  `AppState` are the composition root and **`main` is the first consumer of each** — the binary and
  the tests want the same seam. `crates/worker` has none: its reaper's seams (`sweep_one`,
  `finish_deletions`, `PROCESSING_LEASE`) are private, and making them `pub` would buy nothing but
  the test. So its integration tests sit in-crate under `#[cfg(test)] mod integration`, beside the
  code they drive, sharing `testsupport.rs`. Nothing in the worker was widened to test it.
  **Do not widen visibility just to test something.** No handler, gate or query is `pub` — the tests
  reach them through HTTP, which is the point of `tests/` over an in-crate module. If a *sixth* item
  in `api` needs widening, the lib split has stopped paying: fall back to an in-crate module rather
  than widening further.
- **Verify against the running system, not against this file.** Every claim here was true once; the
  ones that quietly stopped being true are the expensive ones, and they do not announce themselves.
  Recent examples, all found by looking rather than reasoning: the API was streaming the LLM
  gateway's raw error body to browsers; the BFF was sending `bf_session` to the API on every call
  while invariant 20 said it did not; `README` documented `[n]` citation markers that the system
  prompt explicitly forbids. A curl, an echo server, or a captured stream settles in a minute what a
  paragraph can argue for a year.
- **Stop what you started.** A live check leaves things running: `cargo run -p api`, the worker,
  `bun run dev`, a scratch `http.server`, a mock gateway. Kill them when the check is done, and
  revoke any diagnostic key you minted — a stray `sk_` sitting in a tenant is a real credential
  nobody asked for.
  This earns a rule rather than being mere tidiness, because a forgotten binary fails the *next*
  person in a way that hides its own cause. It still holds `:3000`, so `cargo run -p api` dies — but
  every dependency connects first (Postgres, Qdrant, Redis, S3, RabbitMQ all log success), and then
  lapin prints `A Tokio 1.x context was found, but it is being shutdown`, which reads like a broken
  message broker. It is teardown noise: the bind failed, `main` returned `Err`, and the runtime went
  down underneath lapin's io_loop. The cause is the quiet last line — `Address already in use` — not
  the loud error above it. `lsof -nP -iTCP:3000 -sTCP:LISTEN` names the offending pid in a second.
  The five `docker compose` services are the intended dev environment: leave them up unless you
  started them for the check, and say either way.
