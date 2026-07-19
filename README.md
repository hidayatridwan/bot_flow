# bot_flow

A multi-tenant, RAG-based customer service chatbot SaaS. Tenants upload documents; the
platform parses, chunks, embeds and indexes them, then answers end-user questions from
that content — with citations, streamed token by token into an embeddable chat widget.

## Services

The project builds **two Rust binaries** from one Cargo workspace, backed by five
infrastructure containers.

| Service | What it is | Responsibility |
| --- | --- | --- |
| `api` | `crates/api` — Axum HTTP server on `:3000` | Auth, tenant/key admin, uploads, retrieval, LLM answering (incl. SSE streaming). Runs DB migrations at startup. |
| `worker` | `crates/worker` — RabbitMQ consumer | Consumes MinIO `ObjectCreated` events: verifies the object, parses, chunks, embeds, upserts into Qdrant, drives the document lifecycle. Also runs the reaper. |
| `common` | `crates/common` | Shared by API and worker so the two can't drift apart: the object-key contract, the embedding client (`EmbeddingClient`, `EMBEDDING_DIM`), and the chunker (`chunk_text`, `CHUNK_SIZE`, `CHUNK_OVERLAP`) — together, the index recipe. |
| `eval` | `crates/eval` | The retrieval bench (phase 10). Not part of the running system: builds its own index over a committed fixture corpus in its own collection, so a chunking recipe can be measured before an irreversible re-index. |
| `sidecar` | `sidecar/parser.py` | Python text extractor the worker shells out to. Handles `.pdf` (pypdf), `.txt`, `.md`. |
| `postgres` | Postgres 16 | Tenants, API keys, document records, conversation history. Row-Level Security enforces tenant isolation. |
| `qdrant` | Qdrant 1.18 | Vector store. One **versioned** collection (`documents_v2`), partitioned by a `tenant_id` payload index, with a lexical sparse vector written alongside each dense one for phase 10b. |
| `minio` | MinIO (S3 API) | Raw uploaded files, keyed `tenants/{tenant_id}/documents/{document_id}/original.{ext}`. |
| `rabbitmq` | RabbitMQ 3.13 | Carries MinIO's `ObjectCreated` events on a quorum queue with a dead-letter queue. |
| `redis` | Redis 7 | Fixed-window per-tenant rate limiting. |

Upload flow: `POST /documents/upload-url` returns a **presigned PUT**; the client uploads straight to
MinIO, and the API never sees a byte. MinIO publishes `ObjectCreated` to RabbitMQ, and the worker
takes it from there. There is no `/complete` callback for a client to forget or forge.

Ask flow: rewrite the question into a standalone one using conversation history (skipped on the
first turn) → embed it → Qdrant search filtered to the caller's `tenant_id` → drop hits below
`RAG_SCORE_THRESHOLD` → build a numbered CONTEXT prompt → LLM answers in plain prose, or admits it
doesn't know → persist the turn.

The numbering is for the machine, not the reader: the model is **forbidden** from writing `[n]`
markers into its prose, and citations come back as a structured `sources` array instead. That is what
lets a client render them as it likes — or, like today's widget, not at all.

## Tech stack

- **Rust 1.95** (pinned in `rust-toolchain.toml`), Tokio, Axum 0.8, SQLx 0.8, lapin, rust-s3
- **Embeddings**: any OpenAI-compatible `/embeddings` endpoint, running `text-embedding-3-small` —
  1536-dim, cosine. Symmetric, so chunks and questions are embedded verbatim with no prefixes.
  **Every ingested chunk and every question is a billed API call**, authenticated with
  `EMBEDDING_API_KEY` — separate from `LLM_API_KEY` even when both point at the same gateway.
- **LLM**: any OpenAI-compatible `/chat/completions` endpoint. Defaults to Gemini via its
  OpenAI compatibility layer.
- **Chunking**: boundary-aware (paragraph → sentence → clause → word), 500 characters with 60 of
  overlap, UTF-8 safe. Defined once in `common::chunk` — it is half the index recipe, so the worker
  and the retrieval bench cannot drift. The numbers were **measured, not chosen**: see
  [Measuring retrieval quality](#measuring-retrieval-quality).
- **Widget**: dependency-free vanilla JS, ~200 lines, no build step.

## Tenant isolation

Three independent layers, so a single mistake doesn't leak data:

Layers 2 and 3 are no longer claims: `crates/api/tests/tenant_isolation.rs` asserts that one tenant
can neither list, delete, nor *retrieve* another's data, and that the victim tenant still can — see
[Running the integration suite](#running-the-integration-suite).

1. **API key → tenant.** Keys are never stored raw, only as a SHA-256 hash.
2. **Postgres RLS.** `documents` has `FORCE ROW LEVEL SECURITY`. The app connects as the
   non-superuser `app_user` (superusers bypass RLS), and each transaction sets
   `app.current_tenant` via `set_config(…, is_local => true)`. A query that *forgets*
   `WHERE tenant_id = …` still cannot see other tenants' rows.
3. **Qdrant mandatory filter.** Every search is wrapped in a `must` filter on `tenant_id`.

### Two kinds of API key

| Prefix | Kind | Where it lives | Can call |
| --- | --- | --- | --- |
| `sk_…` | secret | your server only | everything below |
| `pk_…` | publishable | shipped to browsers | `/ask`, `/ask/stream` — and only from an `Origin` on the key's allow-list. Asking questions, and nothing else |

`ADMIN_API_KEY` (an env var, not a database row) guards `/admin/*`. A third principal — a **login
session** (a `sess_` bearer token from `/auth/login`) — authenticates the `/auth/*` dashboard routes,
the document-management routes *and* the chat routes; see [Self-serve accounts](#self-serve-accounts).

A session is accepted wherever an `sk_` is, **except** `/ingest` and the deprecated multipart
`POST /documents`. The dashboard has no key to present — the one-time `sk_` is unrecoverable by
design — so the session is the only credential it holds.

**The containment runs one way.** A `pk_` is never accepted where an `sk_` or a session is required:
it is printed in public page source and stays chat-only, which is the whole reason it is safe to
print. The reverse is unremarkable — a session on `/ask` is a stronger credential reaching a route the
weakest one already reaches, which is why the chat routes admit all three.

## Endpoints

| Method | Path | Auth | Notes |
| --- | --- | --- | --- |
| `GET` | `/health` | none | Reachability of all five dependencies; `degraded` if any is down |
| `GET` | `/widget.js` | none | The embeddable widget, served from the binary. `Cache-Control: no-cache` + a strong `ETag`, so a fix reaches every visitor on the next restart |
| `POST` | `/admin/tenants` | admin key | Creates a tenant, returns its first `sk_` key |
| `POST` | `/admin/tenants/{tenant_id}/keys` | admin key | Mints a `secret` or `publishable` key |
| `POST` | `/auth/register` | none | Self-serve: creates a tenant + owner account, returns a session and the first `sk_`. Rate-limited |
| `POST` | `/auth/login` | none | Verifies email + password, returns a session. Rate-limited per email |
| `POST` | `/auth/logout` | session | Ends the current session |
| `GET` | `/auth/me` | session | The account + tenant behind the session |
| `GET` | `/auth/keys` | session | Lists this tenant's key metadata (never the raw key) |
| `POST` | `/auth/keys` | session | Self-serve mint of a `secret`/`publishable` key. Origins are validated and canonicalised; a `publishable` key with none is refused (`422`) |
| `PATCH` | `/auth/keys/{key_hash}` | session | Change a key's `allowed_origins`. `kind` and the hash are immutable |
| `DELETE` | `/auth/keys/{key_hash}` | session | Revokes one of this tenant's keys |
| `POST` | `/documents/upload-url` | secret **or** session | `{"filename": "cv.pdf"}` → a presigned PUT. Rate-limited. Returns `201` |
| `POST` | `/documents/{id}/upload-url` | secret **or** session | Re-mint a URL for a document still `uploading` or `expired`. Only safe for *the same file* — see the note below |
| `POST` | `/documents` | secret | **Deprecated** multipart proxy — buffers the file in the API's memory |
| `GET` | `/documents` | secret **or** session | Lists this tenant's documents and their status |
| `DELETE` | `/documents/{id}` | secret **or** session | Erases the document across Postgres, Qdrant and MinIO. `204` when done inline, `202` when a worker is mid-index and the reaper finishes it. Unknown/other-tenant id → `404` |
| `POST` | `/ingest` | secret | `{"filename","text","external_id?"}` — index text inline, without a file. Creates a real document: the API stores the text as an object and the worker indexes it, so it is listable and erasable like any upload. Returns `202`; poll until `ready`. Rate-limited |
| `POST` | `/search` | secret **or** session | `{"query": "…", "limit": 3}` — returns raw scored chunks, no LLM. Rate-limited. **Not** open to `pk_`: raw retrieval is not asking a question |
| `POST` | `/ask` | any key **or** session | Retrieval + LLM answer as one JSON blob. Rate-limited |
| `POST` | `/ask/stream` | any key **or** session | Same, as SSE: a `conversation` event, a `sources` event, then `token` events, then `done`. An LLM failure yields one `error` event carrying a fixed string — the detail goes to the log, never the client |

Both ask endpoints accept an optional `conversation_id` and return one. See
[Conversation memory](#conversation-memory).

A ready-made Postman collection lives in [postman/](postman/).

## Running locally

**Prerequisites:** Docker, Rust (the toolchain file pins 1.95), Python 3, an API key for any
OpenAI-compatible LLM, and an API key for any OpenAI-compatible `/embeddings` endpoint. The two
may be the same gateway, but they are two separate keys.

### 1. Start the infrastructure

`docker-compose.yml` contains **only** the five backing services — `api` and `worker` run on
your host via Cargo, so you get fast rebuilds.

```bash
docker compose up -d
```

### 2. Set up the Python sidecar

The worker shells out to `sidecar/parser.py`, so it needs pypdf available:

```bash
python3 -m venv sidecar/.venv
sidecar/.venv/bin/pip install -r sidecar/requirements.txt
```

### 3. Configure the environment

Create a `.env` in the repo root. These values line up with `docker-compose.yml`:

```ini
DATABASE_URL=postgres://bot_flow:bot_flow@localhost:5432/bot_flow      # migrations only (superuser)
APP_DATABASE_URL=postgres://app_user:app_user@localhost:5432/bot_flow # runtime, RLS applies
QDRANT_URL=http://localhost:6334                                     # gRPC port, not 6333
RABBITMQ_URL=amqp://bot_flow:bot_flow@localhost:5672/%2f
REDIS_URL=redis://localhost:6379
BIND_ADDR=0.0.0.0:3000
RATE_LIMIT_PER_MINUTE=60
# RAG_SCORE_THRESHOLD=0.25      # optional; 0.25 is the compiled default and was measured, not guessed

LLM_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai
LLM_API_KEY=<your key>
LLM_MODEL=gemini-2.5-flash-lite

EMBEDDING_BASE_URL=https://ai.sumopod.com/v1  # defaults to LLM_BASE_URL if unset
EMBEDDING_API_KEY=<your embedding key>        # a DIFFERENT key from LLM_API_KEY
EMBEDDING_MODEL=text-embedding-3-small        # changing this invalidates every stored vector

S3_ENDPOINT=http://localhost:9000        # what the API and worker use internally
S3_PUBLIC_ENDPOINT=http://localhost:9000 # what CLIENTS connect to; signed into presigned URLs
PRESIGN_TTL_SECS=900
MAX_UPLOAD_BYTES=26214400                # enforced by the worker, not by the presigned PUT
S3_BUCKET=documents
S3_ACCESS_KEY=minio
S3_SECRET_KEY=minio12345
S3_REGION=us-east-1

PARSER_PYTHON=sidecar/.venv/bin/python
PARSER_SCRIPT=sidecar/parser.py

ADMIN_API_KEY=<pick any long random string>
SESSION_TTL_SECS=2592000         # login session lifetime; default 30 days
RUST_LOG=info,api=debug

# Optional. The integration suite derives both from the two URLs above by swapping the database
# name for `bot_flow_test`; set them only to point somewhere else. The second MUST be app_user —
# a superuser bypasses RLS and every isolation test would pass without testing anything.
# TEST_DATABASE_URL=postgres://bot_flow:bot_flow@localhost:5432/bot_flow_test
# TEST_APP_DATABASE_URL=postgres://app_user:app_user@localhost:5432/bot_flow_test
```

Only `BIND_ADDR`, `RATE_LIMIT_PER_MINUTE`, `RAG_SCORE_THRESHOLD`, `SESSION_TTL_SECS`, `LLM_MODEL`,
`EMBEDDING_BASE_URL` (falls back to `LLM_BASE_URL`), `EMBEDDING_MODEL`, `S3_BUCKET`, `S3_REGION` and
the two `PARSER_*` vars have defaults; everything else — `EMBEDDING_API_KEY` included — is required
and the process exits at startup if it's missing.

### 4. Run the two binaries

```bash
cargo run -p api        # terminal 1 — migrates, creates the bucket + collection, serves :3000
cargo run -p worker     # terminal 2 — consumes MinIO upload events (document_events)
```

The API creates the Qdrant collection and the MinIO bucket on boot, and both are idempotent. Nothing
is downloaded at startup — both binaries instead reach `EMBEDDING_BASE_URL` on every ingested chunk
and every question, so an unreachable endpoint or a bad `EMBEDDING_API_KEY` surfaces on first use,
not at boot.

> **Upgrading from a `MultilingualE5Small` checkout?** Those vectors are 384-dim and the collection is
> never rebuilt in place — see [Cutover from the local embedding model](#cutover-from-the-local-embedding-model).

```bash
curl localhost:3000/health
# {"status":"ok","postgres":true,"qdrant":true,"redis":true,"rabbitmq":true,"minio":true}
```

### 5. Run the web dashboard (optional)

A SvelteKit app in `web/`. It is a **BFF**, not a static frontend: the browser never talks to the API
on :3000, and never holds a session token.

```bash
cd web
cp .env.example .env    # API_BASE_URL=http://localhost:3000
bun install
bun run dev             # http://localhost:5173
```

Its own `.env` is separate from the root one and holds **no secrets** — just where to find the API.
Sign up at `/signup`; you land on a page that shows your `sk_` exactly once. Then `/documents` lists
your library and uploads to it. See [`doc/feature/`](doc/feature/) for the design of each phase, and
[`doc/production-readiness.md`](doc/production-readiness.md) for what currently blocks going live.

```bash
bun run test            # unit tests: the validation mirrors, the error maps, the api client, the SSE decoder
bun run check           # svelte-check, strict
```

`bun run lint` is `prettier --check . && eslint .`, and it **fails on a clean checkout**: ~208
vendored `lib/components/ui/` files predate the prettier config, and because the two commands are
`&&`-chained, eslint never runs behind it. Check your own paths — `npx prettier --check <path>`,
`npx eslint <path>` — instead of trusting the summary.

**Run the worker too** if you want uploads to reach `Ready` — the dashboard shows
`Uploading → Processing → Ready` by polling, but it is the worker that does the indexing.

Why the session lives in a cookie on :5173 and not in the browser's JavaScript: the API is
deliberately cookie-free (that is what makes its permissive CORS safe), so SvelteKit exchanges the
login for an `httpOnly` cookie on **its own** origin and forwards it to the API as
`Authorization: Bearer` from the server. An XSS on the dashboard therefore cannot read the token.

**Uploading from the browser does not contradict that.** The bytes go from the browser *straight to
MinIO on :9000*, never through SvelteKit or the API. What the browser receives is a presigned URL,
which authorises one object key, one method, for 15 minutes — a capability, not a credential. The
session stays in Node. This is also why uploading is the one page that requires JavaScript: a
multipart `<form>` would proxy the file through Node, which is exactly the deprecated
`POST /documents` route rebuilt one layer up. Reading the list still works without JS.

Two TypeScript files mirror Rust validators, and both drift into the same failure — the form accepts
something the API then rejects:

- `web/src/lib/features/auth/schema.ts` — the password minimum is counted in **bytes**, like
  `String::len`, and the slug regex is `common::key::is_valid_slug`.
- `web/src/lib/features/documents/schema.ts` — `extensionOf` mirrors `common::key::extension_of`,
  which is `Path::extension()` and **not** `split('.').pop()`: `.pdf` is a dotfile with *no*
  extension and must be rejected, while `..pdf` has one.

Both `schema.test.ts` files port the Rust unit tests verbatim to hold the two languages in step.

### Cutover from the local embedding model

Only if you have an existing checkout that indexed documents with `MultilingualE5Small`. A fresh
install needs none of this.

Those vectors are 384-dim; the new ones are 1536-dim. `ensure_collection()` **early-returns when the
collection already exists**, so it will not rebuild it — the old collection survives the restart and
every upsert fails on a dimension mismatch.

Dropping the collection is not enough on its own. A document whose fingerprint is unchanged is
*skipped* on redelivery (that is what makes redelivery safe), so document rows left at status
`indexed` mean the worker never re-embeds anything and the collection stays **empty forever, with no
error anywhere**. The rows have to go too.

With both binaries stopped:

```bash
# 1. Drop the 384-dim collection.
curl -X DELETE http://localhost:6333/collections/documents

# 2. Truncate the tenant-scoped tables so the documents become re-ingestible.
#    As the migration superuser (DATABASE_URL): under RLS this would match zero rows and
#    report success. Leave `tenants` and `api_keys` alone — that is the tenancy registry,
#    and truncating it invalidates every key you have minted.
psql "$DATABASE_URL" -c 'TRUNCATE messages, conversations, documents;'
```

Then start `cargo run -p api` and confirm it logs `dim=1536`. It recreates the collection with the
`tenant_id` keyword index *before* any ingest can happen — that ordering is load-bearing, because
adding the index after data exists does not retroactively restructure Qdrant's HNSW graph. Re-upload
your documents afterwards.

### Running the integration suite

Unit tests run offline; the integration suite needs the stack. Both are `cargo test`:

```bash
cargo test                      # unit tests — no services, no config, always works
docker compose up -d            # all five: Postgres, Qdrant, MinIO, RabbitMQ, Redis
./scripts/test-setup.sh         # creates the bot_flow_test database (idempotent)
cargo test -- --ignored         # the integration suite
```

### Measuring retrieval quality

`cargo run -p eval` builds its own index over a committed fixture corpus (`crates/eval/fixtures/`)
and scores 44 golden questions — `recall@1/@3/@10`, MRR, and the mean characters of context handed to
the model. It writes to its own `eval_bench` collection and never touches `documents`, but it does
make **real, billed** embedding calls, which is why it is a deliberate command rather than a CI job.

It runs a sabotage table first — deliberately broken variants that each metric must react to — because
a metric that reports the same number whatever the system does is worse than no metric: it is a number
that gets believed. Two lessons from its first run are worth knowing before reading its output:
recall alone cannot see an over-large chunk (returning the whole document always "contains" the
answer), and `recall@3` is saturated at 1.0 on today's small corpus, so `recall@1` and context cost
carry the measurement.

The suite is `#[ignore]`d so a bare `cargo test` stays honest for anyone without Docker — and CI runs
that bare command *before* it starts any service, which is what actually keeps the promise true.
A test that skipped itself when its service was missing would be worse than no test: it would turn
"untested" into "green".

**It needs no LLM or embedding key.** Both gateways are stubbed in-process, so the suite is free,
deterministic and makes no billed call. The stub's embedder is content-addressed — the same string
always yields the same vector — so an exact match scores ~1.0 and unrelated text ~0.0. That margin is
the point: when a tenant retrieves nothing, it can only be the tenant filter, never a threshold.

Two things it refuses to do, both deliberate:

- **It will not run against `bot_flow`.** The database name must end in `_test`. These tests create
  and delete tenants.
- **It will not run as a superuser.** It asserts `NOT rolsuper` and aborts otherwise. Postgres
  superusers bypass RLS entirely, so a suite on the migration role would assert cross-tenant denial,
  pass, and have tested nothing at all. `TEST_DATABASE_URL` and `TEST_APP_DATABASE_URL` override the
  derived URLs if you need them elsewhere; the second one must point at `app_user`.

The worker's integration tests live **inside** `crates/worker/src/` rather than in a `tests/`
directory, and run under the same command. That is deliberate: the reaper's seams are private, and
exporting them purely for a test is the thing not to do. The api's are in `crates/api/tests/` because
its composition root is real API that `main` uses too.

What it covers, and what it does not, is inventoried in CLAUDE.md — including one gap that is
structural rather than merely deferred.

### Resetting the data

[scripts/reset.sh](scripts/reset.sh) wipes every store back to empty — Postgres, MinIO, Qdrant and
Redis — for a clean-slate dev run. It leaves `_sqlx_migrations` and the MinIO bucket's event binding
intact, so the API still boots and uploads still notify the worker.

```bash
./scripts/reset.sh                 # full wipe; prompts for confirmation
./scripts/reset.sh -y              # same, no prompt
./scripts/reset.sh --keep-auth     # keep tenants, api_keys, accounts and sessions
./scripts/reset.sh --keep-test-db  # leave the integration suite's bot_flow_test alone
```

**A full wipe destroys your dashboard logins, not just your documents.** `accounts` and `sessions`
have a foreign key to `tenants`, so `TRUNCATE … CASCADE` reaches them — the script names them in the
prompt rather than letting you discover it at the login screen. `--keep-auth` keeps all four and wipes
only `documents` / `conversations` / `messages`, which is usually what you want mid-development.

It also clears two things that are easy to forget: the integration suite's `bot_flow_test` database
(which accumulates a tenant per test and only self-sweeps debris older than an hour), and the retrieval
bench's `eval_bench` collection. Both are recreated on demand, so there is nothing to restore.

It drops the Qdrant collection rather than recreating it, so **restart `cargo run -p api` afterwards**
— the collection is reborn only at startup, and only then does its `tenant_id` index get created
*before* any ingest, which is load-bearing rather than cosmetic. The script prints what it left behind
so you can see the reset worked; verify the restart with:

```
collection 'documents' created (dim=1536, cosine) + tenant_id index
```

A full wipe also invalidates your `sk_`/`pk_` keys; re-create a tenant to mint new ones (the script
prints the exact curl).

### Local dashboards

| | |
| --- | --- |
| Qdrant | <http://localhost:6333/dashboard> |
| MinIO console | <http://localhost:9001> — `minio` / `minio12345` |
| RabbitMQ management | <http://localhost:15672> — `bot_flow` / `bot_flow` |

## Creating a tenant and keys

Everything below is guarded by `ADMIN_API_KEY`.

The migrations create no tenants and no keys — a fresh database starts completely empty.

```bash
ADMIN=<your ADMIN_API_KEY>

# 1. Create a tenant. The secret key is shown ONCE — only its hash is stored.
curl -sX POST localhost:3000/admin/tenants \
  -H "authorization: Bearer $ADMIN" -H 'content-type: application/json' \
  -d '{"id":"demo","name":"Demo Co"}'
# {"tenant_id":"demo","api_key":"sk_…","note":"store this now; it won't be shown again"}

# 2. Mint a publishable key for the browser, locked to the origins you'll embed on.
curl -sX POST localhost:3000/admin/tenants/demo/keys \
  -H "authorization: Bearer $ADMIN" -H 'content-type: application/json' \
  -d '{"kind":"publishable","label":"website","allowed_origins":["http://localhost:5500"]}'
# {"api_key":"pk_…", …}
```

## Self-serve accounts

The admin flow above is the operator escape hatch. A tenant can also **sign up for themselves** — no
admin key. `POST /auth/register` creates the tenant *and* an owner account (email + password) in one
transaction, and both paths share the same provisioning code so they can't drift. It hands back a
**login session** and the tenant's first `sk_` (shown once, exactly like the admin path).

```bash
# 1. Register. Creates tenant `acme`, an owner account, a session, and the first sk_.
#    `slug` is optional — omit it and it's derived from tenant_name.
curl -sX POST localhost:3000/auth/register -H 'content-type: application/json' \
  -d '{"email":"owner@acme.test","password":"correct horse battery staple","tenant_name":"Acme","slug":"acme"}'
# {"session_token":"sess_…","tenant_id":"acme","api_key":"sk_…","note":"store the api_key now; …"}

# 2. Log in later to get a fresh session (never re-reveals a key). Wrong email and wrong
#    password return the SAME 401 — the endpoint won't tell you which emails exist.
curl -sX POST localhost:3000/auth/login -H 'content-type: application/json' \
  -d '{"email":"owner@acme.test","password":"correct horse battery staple"}'
# {"session_token":"sess_…","tenant_id":"acme"}

SESS=sess_…   # the session_token from above

# 3. Self-serve key management — the dashboard equivalent of the admin /keys route.
curl -s  localhost:3000/auth/me   -H "authorization: Bearer $SESS"   # account + tenant
curl -s  localhost:3000/auth/keys -H "authorization: Bearer $SESS"   # key metadata (never raw)
curl -sX POST localhost:3000/auth/keys -H "authorization: Bearer $SESS" -H 'content-type: application/json' \
  -d '{"kind":"publishable","label":"website","allowed_origins":["http://localhost:5500"]}'
# {"api_key":"pk_…", …}   ← mint your widget's pk_ without the admin key
```

The same flow through a browser is the `web/` dashboard — see
[Run the web dashboard](#5-run-the-web-dashboard-optional). Sessions are Bearer tokens here and
**never cookies**; the cookie is the web app's business, on the web app's origin.

Passwords are Argon2id-hashed and session tokens are SHA-256-hashed — like API keys, a database dump
is not a credential dump. Sessions are **bearer tokens, not cookies**: the browser-facing web app is
expected to be a thin server (a BFF) that trades a login for an httpOnly cookie on its own origin, so
the API's permissive-CORS posture is unaffected. `SESSION_TTL_SECS` sets how long a session lasts
(default 30 days); there is no refresh — an expired session means log in again.

## Feeding it content

Uploads go **directly to MinIO**. The API mints a presigned URL and never touches the bytes, so it
can't become the upload bottleneck.

**Two requests, to two different servers.** Step 1 talks to the API on `:3000`; step 2 uploads to
MinIO on `:9000`. Nothing you send in step 2 passes through the API.

```bash
SK=sk_…

# 1. Ask the API (:3000) for an upload session. Only .pdf, .txt and .md are accepted —
#    the parser supports nothing else, and this is the last point at which a file can
#    be refused, because a presigned PUT cannot inspect content.
curl -sX POST localhost:3000/documents/upload-url \
  -H "authorization: Bearer $SK" -H 'content-type: application/json' \
  -d '{"filename":"handbook.pdf"}'
# {"document_id":"7fe5c124-…",
#  "upload_url":"http://localhost:9000/documents/tenants/acme/…/original.pdf?X-Amz-Signature=…",
#  "method":"PUT","expires_at":"…"}
#                 ^^^^ note the port: 9000. That is MinIO, not the API.

# 2. PUT the raw file at that URL (:9000). No auth header — the signature is in the
#    query string. Expect 200 + an ETag.
curl -X PUT --upload-file handbook.pdf "<paste upload_url here>"

# 3. MinIO tells RabbitMQ; the worker indexes it. Nothing to trigger — just poll.
curl -s localhost:3000/documents -H "authorization: Bearer $SK"
#    status: uploading -> processing -> ready

# If the URL expired before you uploaded, refresh it. Do NOT mint a new session —
# that would orphan the document row you already created.
#
# Refresh re-signs the row's EXISTING object key, whose extension was fixed when the row
# was created — it takes no filename and revalidates nothing. So upload the SAME file you
# asked for. Refresh a `handbook.pdf` row and then PUT a .md, and the bytes land at
# `original.pdf`; the parser dispatches on that suffix, and a perfectly good file fails.
curl -sX POST localhost:3000/documents/$document_id/upload-url -H "authorization: Bearer $SK"

# Or skip the file entirely and hand us the text — it still becomes a real document,
# with a row, a status, and an id you can delete. `external_id` is your own key for the
# source: re-sync the same one and it OVERWRITES rather than piling up duplicates.
curl -sX POST localhost:3000/ingest \
  -H "authorization: Bearer $SK" -H 'content-type: application/json' \
  -d '{"filename":"refunds.md","text":"Refunds are accepted within 30 days of purchase.","external_id":"cms-42"}'
# {"document_id":"…","status":"uploading","note":"indexing is asynchronous; poll GET /documents…"}
#
# Indexing happens in the WORKER, so this returns 202 and not an answer. That is deliberate:
# it means there is exactly one path from text to vectors, and therefore one chunking recipe,
# one payload shape, and one thing to delete.
```

Both `cargo run -p api` and `cargo run -p worker` must be running, or the document sits at
`uploading` forever — the worker is the only thing that ever moves it forward.

### Three ways to get step 2 wrong

- **`-F file=@handbook.pdf` instead of `--upload-file`.** `-F` sends a multipart form body, so MinIO
  stores the MIME envelope *around* your PDF and the parser chokes on it. Easy mistake, because `-F`
  is exactly what the deprecated `POST /documents` endpoint wanted.
- **Adding an `Authorization` header.** MinIO rejects a request carrying both a query-string
  signature and an auth header.
- **Waiting too long.** The URL lives `PRESIGN_TTL_SECS` (900s). Past that MinIO answers
  `403 AccessDenied`; use *Refresh Upload URL*.

### Doing it in Postman

The collection in [postman/](postman/) has the whole flow under **Documents**:

1. **Create Upload URL** — its test script stores `upload_url` and `document_id` as collection
   variables, so you never copy-paste them.
2. **Upload File to MinIO (presigned PUT)** — already set to `PUT {{uploadUrl}}`, **No Auth**, body
   mode **binary**. Open the Body tab and pick your file; Postman can't persist a file path in the
   collection, so that part is once per machine. It asserts `200` and an `ETag`.
3. **List Documents** — poll until `status` is `ready`.

Body mode **binary**, not *form-data* — same trap as `-F` above.

### Document lifecycle

```
                  ┌──────────► expired ──(new url)──┐
                  │                                  │
uploading ────────┴─ ObjectCreated ─► processing ─► ready
    │                                     │
    │                                     ├─► failed ──(retried by the queue)──┐
    └──(re-PUT, new etag ⇒ re-index)◄─────┴─► quarantined (terminal)           │
                                                                               │
                                              └──────────────────────────────◄─┘
```

`uploading` means the row exists but no object has arrived. The reaper expires it once the presigned
URL has lapsed (plus a 5-minute grace, because a signature is checked when a PUT *starts*, so a slow
upload is legitimate). `quarantined` means the object broke a rule no retry can fix — it was over
`MAX_UPLOAD_BYTES` — and the object is deleted.

### Object keys and tenant isolation

```
tenants/{tenant_id}/documents/{document_id}/original.{ext}
```

The key is the only thing binding an upload to a tenant, so `tenant_id` is constrained to
`^[a-z0-9][a-z0-9-]{0,62}$` by both `create_tenant` and a database `CHECK`. Without that, a tenant
registered as `a/../b` could mint a URL whose key escapes its own prefix.

### What a presigned PUT cannot do

**It cannot enforce a size limit.** The signature covers the method, key and expiry — never the body
length. A client holding a valid URL can upload a file of any size, and MinIO will accept it. The
worker checks `MAX_UPLOAD_BYTES` when the event arrives and quarantines the document, but the
bandwidth is already spent. If that becomes a problem, `presign_post` with a `content-length-range`
policy enforces it before the upload starts, at the cost of a form POST instead of a plain PUT.

### Idempotency

A Qdrant point id is `uuid_v5(document_id, chunk_index)` — a hash, not a random draw. Re-indexing a
document therefore *overwrites* its chunks instead of duplicating them, which is what makes a
duplicate `ObjectCreated` event or a RabbitMQ redelivery harmless. Before upserting, the worker also
deletes every point for that `document_id`, so a re-parse yielding fewer chunks can't strand the old
tail.

The worker claims a document with `SELECT … FOR UPDATE` and a status check. A second delivery of the
same event finds `ready` with a matching etag and skips. A *different* etag means the client
overwrote the object, so it re-indexes.

Then ask:

```bash
curl -sX POST localhost:3000/ask \
  -H "authorization: Bearer $SK" -H 'content-type: application/json' \
  -d '{"query":"What is the refund window?"}'
# {"answer":"The refund window is 30 days from the date of purchase.",
#  "sources":[{"index":1,"score":0.54,"document_id":"","text":"Refunds are accepted within 30 days…"}]}
```

Two things in that response are easy to misread. **The prose carries no `[1]`** — the model is
forbidden from writing markers, so `sources[].index` is the only thing tying the answer back to a
passage; it is 1-based and must never be renumbered. And **`score` is 0.54, not 0.9** — that is what a
near-verbatim match actually scores with `text-embedding-3-small`, which is why the threshold note
below matters. (`document_id` is empty here only because this chunk came from `/ingest`; a real
uploaded document reports its id.)

If nothing clears `RAG_SCORE_THRESHOLD` (default `0.25`), the API returns a canned "couldn't find any
relevant information" rather than letting the model guess. The retrieval scores are logged on
every request, and `POST /search` returns them **unfiltered** alongside the configured floor, so you
can see exactly what `/ask` would have dropped.

> **`0.25` is measured, not chosen, and this used to be three different numbers.** The compiled
> default was `0.70` (an E5-era value that made the bot refuse everything with
> `text-embedding-3-small`), README recommended `0.35`, and `.env` said something else again — a
> floor that disagrees with itself in three places is a floor nobody owns.
>
> It was picked by sweeping it on the bench: `0.20` and `0.25` retrieve identically (`0.25` just
> carries ~5% less context), `0.30` starts losing a question, and **`0.35` costs recall@3
> 1.000 → 0.955**. Getting this wrong is silent — refusing when nothing clears the floor is the
> designed behaviour, not an error, so a system that knows nothing looks exactly like one that works.
> Re-sweep with `cargo run -p eval` after any change to the chunker or the embedding model.

## The event pipeline

```
MinIO ──ObjectCreated──► exchange minio.events (direct)
                              │ rk: document.uploaded
                              ▼
                         document_events  (quorum, x-delivery-limit=5)
                              │ 5 failed deliveries
                              ▼
                         doc.dlx ──► document_events.dlq
```

The worker declares all of this at startup, and `minio-init` binds the bucket to the AMQP target.
**Order matters**: a message published to an exchange with no binding is dropped silently, so the
binding must exist before events start flowing.

Three settings do the heavy lifting, and each was chosen for a specific failure:

- **`MINIO_NOTIFY_AMQP_QUEUE_DIR`** — while RabbitMQ is down, MinIO buffers events to disk and
  replays them on reconnect. Without it a broker restart loses every event published during the
  outage, and those documents sit at `uploading` until the reaper expires them. Verified by stopping
  RabbitMQ, uploading, and restarting it: the document still reached `ready`.
- **`x-delivery-limit=5`** on a quorum queue — RabbitMQ counts redeliveries and dead-letters on its
  own, so there's no retry counter in the payload and no TTL/DLX ping-pong queue.
- **`basic_qos(prefetch=1)`** — otherwise the first worker to connect grabs the entire backlog and a
  second worker sits idle.

An event whose key doesn't parse is **acked and logged**, not retried: no amount of redelivery will
make a malformed key parse, and requeueing it would burn the delivery limit of the messages behind it.

The worker reconnects to RabbitMQ indefinitely. It used to exit on a broker restart, which combined
badly with `QUEUE_DIR`: MinIO would faithfully buffer events for a worker that was no longer there.

## Conversation memory

Retrieval embeds the question verbatim, so a follow-up like *"What is his mobile number?"* carries
no semantic link to the document — the pronoun retrieves nothing and the answer comes back as
"couldn't find any relevant information".

To fix that, both ask endpoints keep a conversation. Pass a `conversation_id` and the server loads
the last 10 messages, asks the LLM to rewrite your question into a standalone one, and embeds *that*:

```
"What is his mobile number?"  ->  "What is Ridwan Hidayat's mobile number?"
     scores below the floor              clears the floor, retrieved
```

Omit `conversation_id` and the server mints one, returning it in the response body (`/ask`) or as the
first SSE event (`/ask/stream`). An empty string or `null` means the same thing as omitting it, so a
client holding empty state doesn't have to branch. A non-empty value that isn't a UUID still fails
with `422 Unprocessable Entity` — that's a real bug, not a missing value.

The first turn of a conversation has no history to resolve against, so **the rewrite is skipped
entirely** — no extra LLM call, no added latency. Only follow-ups pay for the second round-trip.

```bash
# Turn 1 — no conversation_id yet.
curl -sX POST localhost:3000/ask -H "authorization: Bearer $SK" -H 'content-type: application/json' \
  -d '{"query":"Who is ridwan hidayat?"}'
# {"answer":"…","sources":[…],"conversation_id":"7dad2af9-…"}

# Turn 2 — pass it back, and the pronoun resolves.
curl -sX POST localhost:3000/ask -H "authorization: Bearer $SK" -H 'content-type: application/json' \
  -d '{"query":"What is his mobile number?","conversation_id":"7dad2af9-…"}'
# {"answer":"His mobile number is +6283141418173.", …}
```

History lives in two RLS-protected tables, `conversations` and `messages`, both scoped by `tenant_id`
exactly like `documents`. A `conversation_id` belonging to another tenant returns `404 conversation
not found` — the same response as an id that never existed, so it can't be used to probe for
existence. Messages are ordered by a `seq` column rather than `created_at`, because both rows of a
turn are written in one transaction and would otherwise share an identical `now()`.

A turn is persisted only once an answer exists. If the LLM fails, nothing is written — otherwise every
failed request would leave a dangling question behind for the next rewrite to reason over.

The widget handles all of this on its own: it records the `conversation` event and replays the id on
every subsequent message. Its "new chat" button clears the id along with the transcript.

## Embedding the widget

**The dashboard writes this snippet for you.** Mint a publishable key at `/keys` and it is rendered
pre-filled with your key and API URL — at mint time, the one moment the raw key exists. What follows
is the same thing by hand.

[widget/widget.js](widget/widget.js) is a self-contained script — no build step, no
dependencies — **served by the API at `/widget.js`**. Point a page at it and initialise it with a
**publishable** key:

```html
<script src="http://localhost:3000/widget.js"></script>
<script>
  ChatWidget.init({
    apiBase:   'http://localhost:3000',
    publicKey: 'pk_…',      // publishable, never a sk_ key
    title:     'BotFlow',   // optional; shown in the widget header
  });
</script>
```

Serving it from the binary is deliberate: tenants used to host their own copy, so a fix could never
reach a deployed site. The API answers `/widget.js` with `Cache-Control: no-cache` and a strong
`ETag`, so a browser revalidates and gets a `304` when nothing changed and the new bytes when it did —
a fix is live for everyone on the next restart, with no snippet edit. `src` and `apiBase` are the same
origin, so there is one thing to configure, not two.

That renders a launcher button in the bottom-right corner which opens a 360px chat panel.
The widget talks to `/ask/stream`, so answers appear token by token, and it **renders citations**:
each `[n]` chip carries the passage's `index` from the API (never renumbered — invariant 5), its
cosine score to two decimals, and the passage text. It shows no filename, because a `pk_` cannot call
`GET /documents` — the chip, score and passage are the honest subset a public key can see.

Two things the browser enforces, and one the server does:

- The page's `Origin` **must** appear in the key's `allowed_origins`, or the API returns
  `403`. Serving from `file://` sends `Origin: null`, which is never allow-listable.
  The comparison is **exact string equality** against what the browser sends — `scheme://host`, plus
  a port only when it is non-default. `POST /auth/keys` canonicalises what you give it
  (`https://Acme.com:443/` → `https://acme.com`) and rejects anything that could never match, because
  a stored origin in the wrong shape is not lax — it is a key that 403s forever.
- The API's CORS policy is deliberately permissive (`allow_origin(Any)`) because the real
  check is the server-side origin allow-list above, and no cookies are involved.
- A `pk_` key is chat-only. Even if someone lifts it from your page's source, it cannot
  upload, list, or ingest documents.

### Trying it out

[widget/demo.html](widget/demo.html) simulates a customer's site. Serve it from an origin
you allow-listed above — the port matters:

```bash
python3 -m http.server 5500 -d widget
# open http://localhost:5500/demo.html
```

Paste your `pk_` key into `demo.html` first, and make sure `http://localhost:5500` was in
the `allowed_origins` you minted the key with.

## Building the container images

The `Dockerfile` is multi-stage with two targets. `api` is a slim Debian image with just the
binary; `worker` additionally carries Python 3 and the sidecar's dependencies.

```bash
docker build --target api    -t bot_flow-api    .
docker build --target worker -t bot_flow-worker .
```
