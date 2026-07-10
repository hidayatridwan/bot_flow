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
| `common` | `crates/common` | The object-key contract shared by API and worker, so the writer and reader can't drift apart. |
| `sidecar` | `sidecar/parser.py` | Python text extractor the worker shells out to. Handles `.pdf` (pypdf), `.txt`, `.md`. |
| `postgres` | Postgres 16 | Tenants, API keys, document records, conversation history. Row-Level Security enforces tenant isolation. |
| `qdrant` | Qdrant 1.18 | Vector store. One `documents` collection, partitioned by a `tenant_id` payload index. |
| `minio` | MinIO (S3 API) | Raw uploaded files, keyed `<tenant_id>/<document_id>`. |
| `rabbitmq` | RabbitMQ 3.13 | Carries MinIO's `ObjectCreated` events on a quorum queue with a dead-letter queue. |
| `redis` | Redis 7 | Fixed-window per-tenant rate limiting. |

Upload flow: `POST /documents/upload-url` returns a **presigned PUT**; the client uploads straight to
MinIO, and the API never sees a byte. MinIO publishes `ObjectCreated` to RabbitMQ, and the worker
takes it from there. There is no `/complete` callback for a client to forget or forge.

Ask flow: rewrite the question into a standalone one using conversation history (skipped on the
first turn) → embed it → Qdrant search filtered to the caller's `tenant_id` → drop hits below
`RAG_SCORE_THRESHOLD` → build a numbered CONTEXT prompt → LLM answers with `[n]` citations, or
admits it doesn't know → persist the turn.

## Tech stack

- **Rust 1.95** (pinned in `rust-toolchain.toml`), Tokio, Axum 0.8, SQLx 0.8, lapin, rust-s3
- **Embeddings**: [fastembed](https://github.com/Anush008/fastembed-rs) running
  `MultilingualE5Small` locally — 384-dim, cosine. No embedding API calls, no per-token cost.
  E5 requires prefixes, so stored chunks are embedded as `passage: …` and questions as `query: …`.
- **LLM**: any OpenAI-compatible `/chat/completions` endpoint. Defaults to Gemini via its
  OpenAI compatibility layer.
- **Chunking**: 800 characters with 100 characters of overlap, UTF-8 safe.
- **Widget**: dependency-free vanilla JS, ~200 lines, no build step.

## Tenant isolation

Three independent layers, so a single mistake doesn't leak data:

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
| `pk_…` | publishable | shipped to browsers | `/ask`, `/ask/stream`, `/search` — and only from an `Origin` on the key's allow-list |

`ADMIN_API_KEY` (an env var, not a database row) guards `/admin/*`.

## Endpoints

| Method | Path | Auth | Notes |
| --- | --- | --- | --- |
| `GET` | `/health` | none | Reachability of all five dependencies; `degraded` if any is down |
| `POST` | `/admin/tenants` | admin key | Creates a tenant, returns its first `sk_` key |
| `POST` | `/admin/tenants/{tenant_id}/keys` | admin key | Mints a `secret` or `publishable` key |
| `POST` | `/documents/upload-url` | secret | `{"filename": "cv.pdf"}` → a presigned PUT. Rate-limited. Returns `201` |
| `POST` | `/documents/{id}/upload-url` | secret | Re-mint a URL for a document still `uploading` or `expired` |
| `POST` | `/documents` | secret | **Deprecated** multipart proxy — buffers the file in the API's memory |
| `GET` | `/documents` | secret | Lists this tenant's documents and their status |
| `POST` | `/ingest` | secret | `{"texts": [...]}` — indexes raw strings, skipping the upload pipeline |
| `POST` | `/search` | any key | `{"query": "…", "limit": 3}` — returns raw scored chunks, no LLM |
| `POST` | `/ask` | any key | Retrieval + LLM answer as one JSON blob. Rate-limited |
| `POST` | `/ask/stream` | any key | Same, as SSE: a `conversation` event, a `sources` event, then `token` events, then `done` |

Both ask endpoints accept an optional `conversation_id` and return one. See
[Conversation memory](#conversation-memory).

A ready-made Postman collection lives in [postman/](postman/).

## Running locally

**Prerequisites:** Docker, Rust (the toolchain file pins 1.95), Python 3, and an API key for
any OpenAI-compatible LLM.

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
RAG_SCORE_THRESHOLD=0.70        # cosine floor for a chunk to count as relevant

LLM_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai
LLM_API_KEY=<your key>
LLM_MODEL=gemini-2.5-flash-lite

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
RUST_LOG=info,api=debug
```

Only `BIND_ADDR`, `RATE_LIMIT_PER_MINUTE`, `RAG_SCORE_THRESHOLD`, `LLM_MODEL`, `S3_BUCKET`,
`S3_REGION` and the two `PARSER_*` vars have defaults; everything else is required and the
process exits at startup if it's missing.

### 4. Run the two binaries

```bash
cargo run -p api        # terminal 1 — migrates, creates the bucket + collection, serves :3000
cargo run -p worker     # terminal 2 — consumes ingest_jobs
```

The **first run of each downloads the embedding model** before it starts serving, so give it
a minute; afterwards it's cached. The API creates the Qdrant collection and the MinIO bucket
on boot, and both are idempotent.

```bash
curl localhost:3000/health
# {"status":"ok","postgres":true,"qdrant":true,"redis":true,"rabbitmq":true,"minio":true}
```

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
curl -sX POST localhost:3000/documents/$document_id/upload-url -H "authorization: Bearer $SK"

# Or skip files entirely and index raw strings:
curl -sX POST localhost:3000/ingest \
  -H "authorization: Bearer $SK" -H 'content-type: application/json' \
  -d '{"texts":["Refunds are accepted within 30 days of purchase."]}'
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
# {"answer":"Refunds are accepted within 30 days [1].","sources":[{"index":1,"score":0.89,…}]}
```

If nothing clears `RAG_SCORE_THRESHOLD` (default `0.70`), the API returns a canned "couldn't find any
relevant information" rather than letting the model guess. The retrieval scores are logged on
every request, so compare them against the floor before assuming retrieval is broken.

Don't raise the floor to `0.80` — `MultilingualE5Small` scores a chunk that verbatim answers the
question at roughly `0.78–0.86`, so `0.80` rejects correct passages. Tune it against your own logged
scores.

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
       cosine 0.79 (rejected)              cosine 0.86 (retrieved)
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
# {"answer":"His mobile number is +6283141418173 [1].", …}
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

[widget/widget.js](widget/widget.js) is a self-contained script — no build step, no
dependencies. Drop it on any page and initialise it with a **publishable** key:

```html
<script src="/path/to/widget.js"></script>
<script>
  ChatWidget.init({
    apiBase:   'http://localhost:3000',
    publicKey: 'pk_…',      // publishable, never a sk_ key
    title:     'BotFlow',   // optional; shown in the widget header
  });
</script>
```

That renders a launcher button in the bottom-right corner which opens a 360px chat panel.
The widget talks to `/ask/stream`, so answers appear token by token, with a citation line
under each reply.

Two things the browser enforces, and one the server does:

- The page's `Origin` **must** appear in the key's `allowed_origins`, or the API returns
  `403`. Serving from `file://` sends no `Origin` header and will be rejected.
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
