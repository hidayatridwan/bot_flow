# RAG Pipeline

> **Scope:** how a document becomes an answer. The parameters below were chosen deliberately —
> read the *why* before changing a number.

Two flows, meeting in Qdrant:

```
INGEST   client → presigned PUT → MinIO → AMQP ObjectCreated → worker → Qdrant
ASK      client → API → rewrite → embed → tenant-filtered search → threshold → LLM → answer
```

---

## The object-key contract

The `common` crate owns the storage key so the writer (API) and the reader (worker) cannot drift:

```
tenants/{tenant_id}/documents/{document_id}/original.{ext}
```

Tenant first, so MinIO policies, lifecycle rules and offboarding can all operate on a prefix. A
directory per document leaves room for derived artifacts (`extracted.txt`) later.

**If you change the key format, change `crates/common/src/key.rs` — never one side.** That crate is
pure string functions with no I/O, which is exactly why it can be tested exhaustively; it has the
densest test suite in the repo. Reading it is the fastest way to understand the upload boundary.

---

## Ingest

### 1. Mint (`POST /documents/upload-url` → `crates/api/src/upload/mod.rs`)

The row is written **before** the URL is minted, so an object can never arrive without a row to
account for it. The reverse — a row with no object — is the abandoned-upload case, settled by the
reaper.

The API never touches file bytes. There is deliberately **no `/complete` callback**: MinIO announces
the finished upload itself, so there is nothing for a client to forget to call, or to forge.

The presigned signature binds the method, the key and the expiry. It does **not** bind body length —
which is why the size cap lives in the worker (see [security-policies.md](security-policies.md)).

`PRESIGN_TTL_SECS` defaults to **900 (15 min)**: long enough for a slow upload, short enough that a
leaked URL rots.

### 2. Event (`crates/worker/src/event.rs`)

MinIO publishes an S3-shaped `ObjectCreated` notification to the `minio.events` exchange with
routing key `document.uploaded`. We do not own that schema, so parsing is defensive — unknown fields
ignored, and the documented traps handled explicitly. The big one:

> **the object key arrives percent-encoded** (`tenants%2Facme%2F…`, and a space as `%20` *or* `+`).

An unparseable key is rejected, never guessed at.

### 3. Claim (`crates/worker/src/lifecycle.rs`)

`SELECT … FOR UPDATE` plus a status check. That lock **is** the entire deduplication story: MinIO can
deliver an event twice, RabbitMQ can redeliver it, and two workers can race for the same document —
only one does the work.

Status machine: `uploading` → `processing` → `ready` | `failed` | `quarantined` | `expired`.

A `ready` document whose etag differs from the event's is **re-indexed** — the client overwrote the
object. Same etag means duplicate delivery: skip.

### 4. Parse (`crates/worker/src/parser.rs` → `sidecar/parser.py`)

Bytes are written to a temp file named by `document_id` (collision-free), the Python sidecar reads
it and writes plain text to stdout. Exit `2` = unreadable, exit `3` = unsupported type.

**Why a Python subprocess:** `pypdf` has no Rust equivalent of comparable maturity, and running an
untrusted-file parser out-of-process contains its failures. It is configured by `PARSER_PYTHON` and
`PARSER_SCRIPT`.

### 5. Chunk (`crates/worker/src/chunk.rs`)

```rust
let chunks = chunk::chunk_text(&text, 800, 100);   // size 800 chars, overlap 100
```

Indexed over **`chars`, never bytes** — slicing UTF-8 by byte offset panics on any non-ASCII
document, and this is a *multilingual* embedding model.

**Why overlap:** a fact that straddles a chunk boundary would otherwise be split into two halves,
neither of which answers the question. 100 characters of overlap means it appears whole in at least
one chunk. Whitespace-only chunks are dropped.

### 6. Embed

`fastembed` running `MultilingualE5Small` **locally**: 384 dimensions, cosine distance. No embedding
API, no per-token cost, no data leaving the host. Weights (~465 MB) download on first run into
`.fastembed_cache/`.

`EMBEDDING_DIM = 384` **must** match the Qdrant collection config. Changing the model means changing
the dimension, recreating the collection, and re-indexing every document.

The model is CPU-bound and behind a `Mutex`. It always runs inside `spawn_blocking`:

```rust
let vectors = tokio::task::spawn_blocking(move || {
    let mut model = embedder.lock().expect("embedder lock poisoned");
    embedding::embed_passages(&mut model, &to_embed)
}).await??;
```

Calling it directly on an async task stalls the whole runtime — every other request on that worker
thread stops until the embedding finishes.

### 7. Index

Two steps, and both are needed:

```rust
// 1. Drop whatever a previous attempt left behind.
ctx.qdrant.delete_points(/* filter: document_id == this */).await?;

// 2. Upsert with deterministic ids.
fn point_id(document_id: &uuid::Uuid, chunk_index: usize) -> uuid::Uuid {
    uuid::Uuid::new_v5(document_id, chunk_index.to_string().as_bytes())
}
```

**Why UUIDv5:** it is a hash, not a random draw, so `(document_id, chunk_index)` always yields the
same id. A redelivered event overwrites its own chunks instead of duplicating them. This is why the
`uuid` dependency carries the `v5` feature, and it is pinned by
`point_ids_are_stable_across_runs` in `crates/worker/src/main.rs`.

**Why the delete is still necessary:** deterministic ids overwrite chunks `0..n`, but a re-parse
yielding *fewer* chunks would strand the old tail. Removing the delete leaves orphaned chunks that
still match searches.

Every point's payload carries `text`, `tenant_id` and `document_id`. `tenant_id` is what
[tenant-isolation.md](tenant-isolation.md) filters on — an unpayloaded point is invisible to every
search and effectively lost.

---

## Ask

### 1. Resolve + rewrite (`crates/api/src/conversation.rs`)

The last `HISTORY_LIMIT = 10` messages are fetched and the LLM rewrites the query into a standalone
question, so pronouns and implicit references become explicit *before* they are ever embedded.
"Does it ship internationally?" embeds to nothing useful; "Does the Pro plan ship internationally?"
retrieves.

The user's **own words** go into history; the rewritten form is only a retrieval and answer key.

Nothing is written during `prepare()`. The turn is persisted only once an answer exists, so a failed
request leaves no trace for the next rewrite to trip over.

### 2. Retrieve

Embed the standalone query, search Qdrant with `tenant_filter()` (mandatory), take `limit` hits
(default **3**).

### 3. Threshold

`RAG_SCORE_THRESHOLD` defaults to **0.70** cosine similarity. From `config.rs`:

> MultilingualE5Small scores a chunk that verbatim answers the question around **0.78–0.86**, so
> anything at or above 0.80 rejects correct passages. Tunable without recompiling — watch the logged
> retrieval scores and adjust.

If nothing survives the threshold, the pipeline **stops here**. No LLM call is made. The response is
`200` with `answer: NO_ANSWER` and `sources: []`, and the turn is still recorded.

### 4. Prompt

Surviving chunks are numbered and joined:

```
CONTEXT:
[1] …
[2] …

QUESTION: <standalone query>
```

`RAG_SYSTEM_PROMPT` constrains the model to answer **only** from the numbered context, to say
honestly when the answer is not there, and — specifically — to never emit citation markers or
bracketed numbers in its prose. The `[1] [2]` numbering exists for the machine, not the reader; the
`sources[].index` field is how a client maps an answer back to a passage.

**Why so strict:** this is a customer-service bot answering as the tenant's business. A hallucinated
refund policy is worse than "I don't know."

### 5. Stream (`/ask/stream`)

Same pipeline, SSE-framed. Event order is a contract — see [api-standards.md](api-standards.md).

---

## Failure handling

The worker classifies every failure, and the classification decides the queue's behaviour:

| | Meaning | Action |
| --- | --- | --- |
| `Fatal` | retrying can never make it succeed (unparseable event, unroutable key) | **ack** — discard the poison message |
| `Retryable` | transient (Qdrant restart, LLM blip, MinIO hiccup) | **nack, requeue** — the quorum queue's `x-delivery-limit` dead-letters after 5 tries |

Acking the first kind is what keeps a poison message from cycling forever. Nacking the second is what
lets a transient failure recover on its own. **An unclassified error is a bug** — see
[code-conventions.md](code-conventions.md).

Oversize objects are `quarantined` (terminal, bytes deleted) rather than `failed` — no retry can make
a 40 MB file smaller.

Dead letters land in `document_events.dlq` via the `doc.dlx` exchange.

### The reaper (`crates/worker/src/reaper.rs`)

Settles rows no event will ever arrive for:

- `uploading` past `upload_expires_at` + **5 min grace** → `expired`. The grace absorbs clock skew
  and slow uploads: a signature is checked when the PUT *starts*, so a transfer that began just
  before expiry is legitimate and may still be in flight.
- `processing` held longer than the **30 min lease** → the worker died; reclaim it.

It sweeps **per tenant in a loop**, because RLS makes a single cross-tenant `UPDATE` match zero rows
and report success.

An `expired` row is not dead: `POST /documents/{id}/upload-url` re-mints a URL for it. Without that,
an expired upload would permanently burn an object key nothing could ever be written to.

---

## Tuning cheatsheet

| Knob | Default | Where | Change when |
| --- | --- | --- | --- |
| chunk size / overlap | 800 / 100 | `worker/src/main.rs` (call site) | answers cite half-sentences, or context blows the LLM window |
| `limit` | 3 | `handlers.rs::default_limit` | answers miss facts spread across many chunks |
| `RAG_SCORE_THRESHOLD` | 0.70 | env | too many "I don't know" (lower) / irrelevant citations (raise) |
| `HISTORY_LIMIT` | 10 | `conversation.rs` | follow-ups lose the thread |
| `PRESIGN_TTL_SECS` | 900 | env | large files time out mid-upload |
| `MAX_UPLOAD_BYTES` | 25 MiB | env | — |

Changing chunk size or the embedding model **requires re-indexing every document**. Old chunks keep
their old shape; a mixed collection retrieves badly and there is no error to tell you.

---

## Maintenance

Review when the model, the chunker, the prompt, or the queue topology changes — and at sprint
planning. Treat edits as production changes. The numbers in this file are duplicated in code; if you
change one, change both in the same commit.

Related: [tenant-isolation.md](tenant-isolation.md), [api-standards.md](api-standards.md),
[code-conventions.md](code-conventions.md).
