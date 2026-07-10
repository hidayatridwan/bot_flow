# bot_flow — System Specification

> **Status:** Single Source of Truth for business logic and system architecture.
> **Rule:** a change to system behaviour is defined *here first*, then implemented. If the code and
> this document disagree, one of them is a bug — decide which, in that order.

This document describes **what must be true** of `bot_flow` and **why**. It deliberately contains no
implementation detail: no function signatures, no line numbers, no code. For how the rules are
enforced in Rust, see [`steering/`](../steering/). For how to run the system, see
[`README.md`](../README.md).

---

## 1. System Overview

`bot_flow` is a **multi-tenant, RAG-based customer-service chatbot SaaS**.

A tenant is a business. It uploads its own support material — PDFs, plain text, Markdown. The
platform extracts the text, splits it into overlapping chunks, converts each chunk into a vector, and
indexes it. That tenant's end users then ask questions through a chat widget embedded on the tenant's
own website, and receive answers drawn **only** from that tenant's documents, with citations,
streamed word by word.

The product promise is narrow and load-bearing: **the bot answers from your documents, or it says it
doesn't know.** It does not answer from the language model's general knowledge, and it never sees, let
alone answers from, another tenant's material. Every architectural decision below serves one of those
two guarantees.

### Shape of the system

Two long-running processes, built from one Rust workspace:

| Process | Responsibility |
| --- | --- |
| **API** (`crates/api`) | Authenticates callers, administers tenants and keys, mints upload URLs, retrieves and answers questions, streams responses. Runs database migrations at startup. |
| **Worker** (`crates/worker`) | Consumes upload events. Parses, chunks, embeds and indexes documents. Drives the document lifecycle. Reclaims stuck documents. |

They share one library, `crates/common`, which owns the storage-key format — the single contract both
sides must agree on. A Python sidecar (`sidecar/`) extracts text from PDFs.

Five backing services: **Postgres** (tenants, keys, documents, conversations; enforces isolation),
**Qdrant** (vectors), **MinIO** (uploaded files), **RabbitMQ** (upload events), **Redis** (rate
limiting).

Embeddings are computed **locally**, on the host, with a small multilingual model. There is no
embedding vendor, no per-token embedding cost, and no document text sent to a third party during
indexing. The answering model is any OpenAI-compatible chat endpoint, and it sees only the handful of
chunks selected for a given question.

---

## 2. Business Logic & Invariants

These are the rules of the game. Each is numbered so it can be cited in a code review, a commit
message, or a future spec. **Breaking one is not a bug to be weighed against other bugs — it is a
product failure.**

Each invariant records *where* it is enforced, because "the application checks it" and "the database
refuses it" are very different guarantees.

### Tenancy

**INV-1 — A tenant can never observe another tenant's data.**
Enforced in three independent layers, deliberately redundant:
1. every vector search carries a mandatory tenant filter (application);
2. the tenant-scoped tables deny by default under Row-Level Security, so a query that *forgets* to
   scope itself returns nothing rather than everything (database);
3. the runtime connects as a non-privileged role, because a superuser bypasses Row-Level Security
   entirely (deployment).

Any one layer would be sufficient on a good day. All three exist because a single mistake — one
forgotten filter in one handler — must not be sufficient to leak a customer's documents.

**INV-2 — A forgotten tenant scope fails closed, never open.**
Row-Level Security compares against a per-transaction setting. When that setting is absent the
comparison is not true, so the result is *zero rows*. A developer who forgets the scope sees an empty
list and investigates. They never see someone else's data.

The corollary is a trap worth stating: **a cross-tenant bulk update matches zero rows and reports
success.** Any maintenance operation must iterate tenant by tenant. Silence is not confirmation.

**INV-3 — The storage key is the authorisation boundary of an upload.**
An uploaded object lives at `tenants/{tenant_id}/documents/{document_id}/original.{ext}`. A presigned
URL authorises exactly one key and nothing else, so the key *is* the permission.

Therefore a tenant identifier is constrained to `^[a-z0-9][a-z0-9-]{0,62}$` — enforced both by the
application (to return a clear error) and by a database constraint (so no future code path can
bypass it). A tenant registered as `a/../b` could otherwise mint a URL whose key escapes its own
prefix into a neighbour's.

Each object key is unique across the system: two document records must never claim the same object.

### Answering

**INV-4 — An answer is grounded in retrieved context, or it is refused.**
Retrieved chunks scoring below the relevance floor are discarded. If nothing survives, the system
returns a fixed "couldn't find that information" response and **does not call the language model at
all**. The model is never given the opportunity to fill a silence.

The reasoning: this bot answers *as the tenant's business*. A hallucinated refund policy is worse
than an admission of ignorance, because the customer will act on it and the tenant will be held to
it.

**INV-5 — The model may only use the passages it is given.**
The system prompt confines the model to the numbered context passages and instructs it to admit when
the answer is absent. It is further instructed never to write citation markers into its prose: the
numbering exists for the machine, and citations are returned as structured data alongside the answer,
not embedded in it.

**INV-6 — Retrieval quality depends on asymmetric embedding prefixes.**
The embedding model requires stored chunks and questions to be encoded differently. Using the wrong
prefix does not error — it silently degrades retrieval. This is a correctness rule wearing the
costume of a formatting detail.

**INV-7 — A conversation turn is recorded only once an answer exists.**
If retrieval or the model fails, nothing is written. Otherwise every failed request would leave a
dangling question in the history, and the *next* question's rewrite would reason over it.

**INV-8 — An unknown conversation and another tenant's conversation are indistinguishable.**
Both return "not found". The alternative — "forbidden" for one and "not found" for the other — would
turn the endpoint into an oracle for discovering which conversation identifiers exist.

### Ingestion

**INV-9 — Indexing the same document twice is a no-op, not a duplication.**
A chunk's vector identifier is derived deterministically from its document and its position, so
re-indexing overwrites in place. Before writing, the worker removes every existing vector for that
document, because a re-parse that yields *fewer* chunks would otherwise strand the old tail as
orphans that still match searches.

This is what makes the event pipeline safe: the message broker may redeliver, and the storage layer
may announce the same upload twice. Neither can corrupt the index.

**INV-10 — A document is claimed by exactly one worker.**
A row lock plus a status check is the entire deduplication story. A second delivery finds the
document already finished — with an identical object fingerprint — and skips. A *different*
fingerprint means the client overwrote the file, so it is re-indexed.

**INV-11 — Upload size cannot be enforced at upload time, and pretending otherwise is dangerous.**
A presigned upload's signature covers the method, the key and the expiry. It does **not** cover the
body length. A client holding a valid URL can upload a file of any size and storage will accept it.

The size limit is therefore enforced *after the fact*, by the worker, when the upload event arrives.
The bandwidth is already spent; all the system can do is refuse to keep the object. Oversize
documents are quarantined and their bytes deleted. Do not "move this check earlier" — there is no
earlier.

**INV-12 — Every upload has a document record before it has a URL.**
The record is created first, so an object can never arrive that nothing accounts for. The reverse —
a record whose upload never arrived — is an expected state, and is settled by the reaper.

**INV-13 — There is no upload-completion callback.**
Storage announces the completed upload itself. A client cannot forget to call it, and cannot forge a
call to it.

### Credentials

**INV-14 — API keys are stored as one-way hashes and are never logged.**
Only a SHA-256 hash reaches the database. The raw key is displayed exactly once, in the response that
mints it, and is thereafter unrecoverable — by the tenant, by support, and by anyone who obtains a
database dump.

This is a deliberate trade: we permanently lose the ability to show a customer their key again, in
exchange for a stolen database not being a stolen set of credentials.

**INV-15 — A publishable key is chat-only, and is bound to an origin.**
Two key kinds exist:

| Kind | Prefix | Lives | May do |
| --- | --- | --- | --- |
| Secret | `sk_` | on the tenant's server | everything |
| Publishable | `pk_` | in a public web page's source | ask questions, and only from an allow-listed origin |

A publishable key is *expected* to be stolen — it is printed in the page source of every visitor. Its
containment is that it can only ask questions, and only from origins the tenant nominated when
minting it. Management endpoints reject it outright.

Administrative operations (creating tenants, minting keys) are guarded by a deployment-level secret,
not a database row, because they are the operations that *create* database rows.

**INV-16 — Internal failure detail never reaches a client.**
An unexpected error is logged in full and answered with a generic message. Errors caused by the
caller carry a message describing *the caller's mistake*, and nothing about the system's internals.

---

## 3. Data Flow & Lifecycle

### 3.1 The document lifecycle

A document is the primary object in the system. Its states:

| State | Meaning | Terminal? |
| --- | --- | --- |
| `uploading` | record exists, no file has arrived | no |
| `processing` | a worker holds it and is indexing | no |
| `ready` | indexed and searchable | no (a re-upload re-indexes) |
| `failed` | indexing failed for a transient reason; the queue will retry | no |
| `expired` | the upload window lapsed and nothing arrived | no (a new URL revives it) |
| `quarantined` | the file broke a rule no retry can fix; its bytes are deleted | **yes** |

```
                    ┌──────────► expired ──(new upload url)──┐
                    │                                        │
uploading ──────────┴── file arrives ──► processing ──► ready
    │                                        │
    │                                        ├──► failed ──(queue retries)──┐
    └──(re-upload, new fingerprint ⇒ re-index)◄──┴──► quarantined           │
                                                                            │
                                          └─────────────────────────────────┘
```

The distinction that matters: **`failed` is a promise to try again; `quarantined` is a refusal.** A
transient storage outage produces the former. A file over the size limit produces the latter, because
no number of retries makes a file smaller.

### 3.2 Ingesting a document

1. **Mint.** The tenant asks the API for an upload session, naming a filename. The extension is
   validated against what the parser can actually read — this is the *last* moment a file can be
   refused, because a presigned upload cannot inspect content. A document record is created
   (`uploading`) and a time-limited upload URL is returned.
2. **Upload.** The client sends the bytes **directly to object storage**. They do not pass through
   the API, which therefore cannot become the upload bottleneck and never buffers a file in memory.
3. **Announce.** Object storage publishes an upload event onto the message broker. Events are
   buffered to disk while the broker is unavailable, so a broker restart does not lose uploads.
4. **Claim.** A worker takes the event, verifies the object matches what the event claimed, checks
   the size limit, and atomically claims the document (`processing`).
5. **Extract.** The file is handed to the Python sidecar, which returns plain text. Running an
   untrusted-file parser out-of-process contains its failures.
6. **Chunk.** Text is split into overlapping windows. Overlap exists so that a fact straddling a
   boundary still appears whole in at least one chunk.
7. **Embed & index.** Each chunk becomes a vector, tagged with its tenant and document, and is
   written to the vector store under a deterministic identifier (INV-9).
8. **Settle.** The document becomes `ready`.

**Failure handling.** Every failure is classified before it is acted on:

| Class | Meaning | Response |
| --- | --- | --- |
| **Fatal** | retrying can never succeed — an unparseable event, a malformed key | discard the message |
| **Retryable** | transient — a service restarted, the network blinked | return it to the queue |

Discarding the first class is what stops a poison message from cycling forever and starving the
messages behind it. Returning the second is what lets an outage heal itself. After a bounded number
of failed deliveries the broker moves the message to a dead-letter queue rather than retrying
indefinitely. Workers take one message at a time, so a backlog spreads across every available worker
instead of being swallowed whole by the first.

**An unclassified failure is a defect**, not a default.

### 3.3 The reaper

Some documents wait for an event that will never arrive. A background sweep settles them:

- A document still `uploading` after its URL expired — plus a grace period — becomes `expired`. The
  grace exists because an upload's signature is checked when the transfer *starts*: a large file that
  began just before expiry is legitimate and may still be in flight.
- A document held in `processing` beyond a lease period is assumed to belong to a worker that died,
  and is reclaimed.

An `expired` document is not dead. Its upload URL can be re-minted. Without that, an expired upload
would permanently burn an object key that nothing could ever write to (INV-3: keys are unique).

The sweep runs **once per tenant**, because of INV-2's corollary.

### 3.4 Answering a question

1. **Authenticate** the caller to a tenant, and enforce the per-tenant rate limit.
2. **Resolve the conversation.** An absent, empty or null conversation identifier means "start a new
   one" — an empty string is what an untouched form field sends, and rejecting it would fail the very
   first request of every conversation, which is precisely the one that creates it.
3. **Rewrite the question** against recent history, so that pronouns and implicit references become
   explicit *before* the question is embedded. *"Does it ship internationally?"* retrieves nothing;
   *"Does the Pro plan ship internationally?"* retrieves. The user's original words go into history;
   the rewritten form is only a retrieval key.

   The first turn has no history to resolve against, so **the rewrite is skipped entirely** — no
   extra model call, no added latency. Only follow-ups pay for it. If the rewriter fails or returns
   something unusable, the system falls back to the raw question: a worse retrieval key, but a safe
   one. **A rewriter outage must not take answering down with it.**
4. **Retrieve** the closest chunks, filtered to this tenant (INV-1).
5. **Filter** by the relevance floor. If nothing survives: refuse (INV-4), record the turn, stop.
6. **Answer.** Surviving chunks are numbered and given to the model as context (INV-5).
7. **Record** the turn — question and answer together, in one transaction (INV-7).

The streaming variant performs the same pipeline and emits the conversation identifier first, then
the sources, then the answer word by word, then a terminal completion signal. A client must capture
the conversation identifier before the first word arrives, because that identifier is how the next
turn continues the conversation.

### 3.5 Conversation memory

History is stored per tenant, under the same isolation rules as documents. Only a bounded number of
recent messages inform a rewrite; conversations do not grow unboundedly into the prompt.

Messages are ordered by an explicit sequence, not by their timestamp: both messages of a turn are
written in one transaction and would otherwise share an identical creation time, leaving their order
undefined.

---

## 4. Key Technical Standards

### 4.1 Authentication

Every endpoint except the health check requires a bearer credential. The credential resolves to a
tenant and a key kind; publishable keys are additionally checked against their origin allow-list.
Administrative endpoints compare against a deployment secret and carry no tenant.

### 4.2 HTTP status codes

| Code | Meaning here |
| --- | --- |
| `200` | success |
| `201` | a tenant or key was created |
| `202` | work accepted for later processing (deprecated upload path only) |
| `400` | the request is malformed in a way we detected ourselves — bad slug, unsupported file type |
| `401` | credential missing, malformed, or unknown |
| `403` | a publishable key on a secret-only endpoint, or a disallowed origin |
| `404` | unknown tenant, or a conversation/document not visible to this tenant (INV-8) |
| `409` | tenant identifier already taken |
| `422` | the body parsed as JSON but failed validation — e.g. a non-UUID conversation identifier |
| `429` | per-tenant rate limit exceeded |
| `500` | an unexpected internal failure; detail is logged, never returned (INV-16) |

Note the `400` / `422` split: a syntactically broken body is `400`; a well-formed body carrying a bad
value is `422`. This is the framework's convention and clients depend on it.

### 4.3 Response shapes

Errors are always `{"error": "<message>"}`.

An answer carries the prose, an ordered list of sources (each with its position, relevance score,
originating document, and the quoted text), and the conversation identifier. **The source position is
the only way a client can map an answer back to a passage**, because the model is forbidden from
writing citation markers into its prose (INV-5). It is one-based. Do not renumber it.

A "no relevant information" outcome is a **successful response** carrying the refusal and an empty
source list. It is not an error and not a `404` — "I don't know" is a correct answer.

### 4.4 The streamed event sequence

`conversation` (once, first) → `sources` (once) → `token` (zero or more) → `done` (once, terminal).
A failure emits `error`, which is also terminal. A stream that ends without `done` or `error` means
the connection dropped, not that the answer finished.

### 4.5 Naming and identifiers

- **Tenant identifier**: a human-chosen slug, `^[a-z0-9][a-z0-9-]{0,62}$` (INV-3).
- **Document identifier**: a random UUID, minted by the API.
- **Vector identifier**: derived deterministically from document and chunk position (INV-9).
- **API key**: prefix (`sk_` / `pk_`) plus high-entropy random material. The prefix is a human aid;
  the authority comes from the hash lookup.
- **Object key**: `tenants/{tenant_id}/documents/{document_id}/original.{ext}` — tenant first, so that
  storage policies, lifecycle rules and tenant offboarding can all operate on a prefix. A directory
  per document leaves room for derived artefacts later.

### 4.6 Configuration

Behavioural constants — the relevance floor, the rate limit, the upload size cap, the upload URL
lifetime, the answering model — are environment configuration, not compile-time constants. They are
tuned against production logs without a rebuild. Required configuration is validated at startup and
the process refuses to start without it, rather than failing at first use.

One configuration value is a recurring source of confusion and deserves naming here: the address at
which **clients** reach object storage may differ from the address the API uses internally, and
upload URLs must be signed against the former. In local development they coincide.

---

## 5. Known State & Technical Debt

Honest inventory. Each entry states the impact, not merely the fact.

### 5.1 The raw-text ingest endpoint violates the document model

`POST /ingest` writes vectors directly from supplied strings. Unlike the upload path, those vectors
receive **random identifiers** and carry **no document reference**.

Three consequences, all real and all currently unhandled:

- Ingesting the same text twice **duplicates** the vectors. INV-9 does not hold on this path.
- The resulting chunks appear in answers with an **empty document reference**, so a client cannot
  attribute the citation to anything.
- They belong to no document record, so they can never be listed, re-indexed, or removed. They are
  permanent.

Tenant isolation *is* preserved — the tenant tag is written and the search filter applies — so this
is not a data-leak risk. It is a data-lifecycle hole. **This is the largest single piece of debt in
the system.** Treat `/ingest` as a demo and testing convenience, not a supported ingestion path, and
do not build on it.

### 5.2 There is no way to delete a document

Nothing removes a document record, its vectors, or its stored bytes. A tenant that uploads a file has
no way to un-upload it, and a "delete my data" request cannot presently be honoured. For a product
holding customers' support documents, this is a compliance gap, not merely a missing feature.

Designing it means deciding the order of operations across three stores (record, vectors, bytes) and
what happens when a step fails halfway — which is exactly the kind of decision this document exists
to capture *before* the code is written.

### 5.3 The multipart upload endpoint is deprecated

`POST /documents` proxies the whole file through the API, buffering it in memory. It exists only
until clients migrate to the presigned-upload flow, and is removed together with the API's legacy
queue module and the worker's legacy consumer. **Do not add features to it. Do not add callers.**

### 5.4 Two endpoints are unmetered, and one is under-authenticated

`POST /search` accepts a publishable key and has no rate limit — meaning a key printed in a public
web page can drive unlimited vector searches. `POST /ingest` is also unmetered.

This is believed to be an oversight rather than a decision. Nothing in the code records an intent.
**Confirm the intent before relying on either behaviour**, and if it is closed, update §4 in the same
change.

### 5.5 A document state exists that nothing ever writes

The database permits an `uploaded` status which no code path assigns. It is a vestige. Either give it
meaning or remove it from the constraint; an unreachable state in a lifecycle is a trap for the next
person to read the schema.

### 5.6 Vector storage has no migration path

The database has ordered, forward-only migrations. The vector store has nothing equivalent.

Changing the embedding model, or its dimension, or the chunking parameters, invalidates every stored
vector. There is no rollback — only recreating the collection and re-indexing every document of every
tenant. Worse, a **partially** re-indexed collection produces quietly degraded retrieval with no
error anywhere. Any such change is a migration project, not a configuration change.

### 5.7 The isolation guarantee is untested

There is no automated test asserting that Row-Level Security actually denies a cross-tenant read.
INV-1 is the system's most important promise and it currently rests on code review alone.

More broadly: there is no continuous integration, and no integration test suite. Tests are unit tests
over pure functions. The highest-value missing tests, in order: cross-tenant denial; concurrent claim
of one document by two workers; origin rejection for publishable keys; correct fatal-versus-retryable
classification.

### 5.8 No example environment file

The ignore rules anticipate a committed `.env.example`, but none exists. A new contributor has to
reconstruct the required configuration from source. Names only, values blank — the file is trivial to
write and its absence is pure friction.

---

## 6. Amending This Document

1. **Change the specification first.** A behaviour change begins as an edit here — a new invariant, a
   revised lifecycle state, a closed gap in §5.
2. **Then design.** For anything non-trivial, write the design and an ordered task list under
   `specs/<feature>/` (see [`specs/README.md`](README.md)). Review the task order before any code is
   generated: the data model precedes the endpoint that exposes it.
3. **Then implement**, following [`steering/`](../steering/) for how code is written here.
4. **Then reconcile.** If implementation revealed the specification was wrong, fix the specification
   in the same change. A document that quietly drifts from the code is worse than no document,
   because it is trusted.

Invariant numbers are stable references. Retire one by marking it withdrawn and saying why; do not
renumber the rest.
