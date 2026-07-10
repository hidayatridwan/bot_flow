# API Standards

> **Scope:** HTTP surface of `crates/api` — handler shape, auth, errors, status codes, response
> bodies, SSE. Not deployment, not security rationale (see
> [security-policies.md](security-policies.md)).

The API is [Axum](https://docs.rs/axum) 0.8. Routes are declared in one place —
`crates/api/src/main.rs` — and implemented in `crates/api/src/handlers.rs`.

---

## Handler signature

```rust
pub async fn ask(
    State(state): State<AppState>,   // 1. state
    tenant: AuthTenant,              // 2. FromRequestParts — reads headers, no body
    Json(req): Json<AskRequest>,     // 3. body extractor — ALWAYS LAST
) -> Result<Json<Value>, AppError> {
```

**Extractor order is load-bearing.** `FromRequestParts` extractors (`AuthTenant`, `AdminAuth`,
`Path`, `State`) must come *before* the body extractor (`Json`, `Multipart`).

**Why:** an Axum handler consumes the request body exactly once, so only the final argument may
implement `FromRequest`. Put `Json` first and you get a trait-bound error about
`Handler<_, _>` not being satisfied — a message that says nothing about argument order and sends
you hunting through your types for an hour. The existing handlers carry an inline
`// FromRequestParts — reads headers, no body` comment for exactly this reason. Keep it.

Return type is `Result<Json<Value>, AppError>`, or `Result<(StatusCode, Json<Value>), AppError>`
when the status is not `200`.

---

## Error contract

One error type, two classes — `crates/api/src/error.rs`:

```rust
pub enum AppError {
    Internal(anyhow::Error),        // → 500, logged in full, client sees a generic message
    Client(StatusCode, String),     // → 4xx, message is safe to show the caller
}
```

`Internal` responds with `{"error": "internal server error"}` and nothing else. The real error goes
to `tracing::error!`. **Internal detail is never returned to a client** — not the SQL, not the
connection string, not the upstream body.

There is a blanket impl:

```rust
impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self { Self::Internal(err.into()) }
}
```

This is what lets handlers use bare `?` on `sqlx`, `reqwest` and `qdrant` errors. It also means:

> **A bare `?` always produces a 500.** If the failure is the *caller's* fault, `?` gives them the
> wrong answer. Client errors must be constructed explicitly.

### Before / After

```rust
// ❌ BEFORE — a caller who passes an unknown tenant gets an opaque 500, and the on-call
//    engineer gets paged for a foreign-key violation that was never a server problem.
sqlx::query("INSERT INTO api_keys (key_hash, tenant_id, ...) VALUES ($1, $2, ...)")
    .bind(auth::hash_key(&raw))
    .bind(&tenant_id)
    .execute(&state.db)
    .await?;                       // fkey violation -> AppError::Internal -> 500

// ✅ AFTER — the real handler. Check first, and say what the caller got wrong.
let exists = sqlx::query("SELECT 1 FROM tenants WHERE id = $1")
    .bind(&tenant_id)
    .fetch_optional(&state.db)
    .await?
    .is_some();
if !exists {
    return Err(AppError::client(
        StatusCode::NOT_FOUND,
        format!("tenant '{tenant_id}' does not exist; create it first"),
    ));
}
```

Rule of thumb: **`?` for anything the caller could not have prevented; `AppError::client` for
anything they could.**

---

## Status codes in use

| Code | When |
| --- | --- |
| `200` | successful read / ask / search |
| `201 CREATED` | `POST /admin/tenants`, `POST /admin/tenants/{id}/keys` |
| `202 ACCEPTED` | deprecated `POST /documents` — work queued, not done |
| `400 BAD REQUEST` | malformed slug, unsupported file extension, bad `kind` |
| `422 UNPROCESSABLE ENTITY` | body parsed as JSON but failed deserialization (Axum's `JsonRejection`) |
| `401 UNAUTHORIZED` | missing / malformed / unknown `Authorization` header |
| `403 FORBIDDEN` | publishable key on a secret-only endpoint; disallowed `Origin` |
| `404 NOT FOUND` | unknown tenant on key mint; no document awaiting upload |
| `409 CONFLICT` | tenant id already exists |
| `429 TOO MANY REQUESTS` | per-tenant rate limit exceeded |
| `500` | anything `AppError::Internal` |

---

## Endpoints

`sk_` = secret key required · `pk_` = publishable key accepted · `admin` = `ADMIN_API_KEY`

| Method | Path | Auth | Rate limited | Notes |
| --- | --- | --- | --- | --- |
| `GET` | `/health` | none | no | probes all 5 dependencies concurrently |
| `POST` | `/ingest` | `sk_` | **no** | raw-text ingest |
| `POST` | `/search` | `sk_` or `pk_` | **no** | vector search, no LLM |
| `POST` | `/ask` | `sk_` or `pk_` | yes | RAG answer |
| `POST` | `/ask/stream` | `sk_` or `pk_` | yes | SSE, used by the widget |
| `GET` | `/documents` | `sk_` | no | list with status |
| `POST` | `/documents` | `sk_` | yes | **DEPRECATED** — see below |
| `POST` | `/documents/upload-url` | `sk_` | yes | mint presigned PUT |
| `POST` | `/documents/{document_id}/upload-url` | `sk_` | yes | re-mint an expired PUT |
| `POST` | `/admin/tenants` | admin | no | returns the tenant's first `sk_` key, once |
| `POST` | `/admin/tenants/{tenant_id}/keys` | admin | no | mint additional keys |

> **Known inconsistency, do not "fix" silently.** `/ingest` and `/search` accept work without a
> `rate_limit::check`, and `/search` does not call `require_secret()` — so a browser-facing `pk_`
> key can run unmetered vector searches. This is a real gap, not a documented decision. If you are
> touching these handlers, raise it; if you close it, update this table in the same change.

New endpoints must call `tenant.require_secret()?` unless they are deliberately part of the
browser-facing chat surface, and should call `rate_limit::check(&state, &tenant.tenant_id).await?`
before doing expensive work.

---

## Response bodies

Bodies are built with `serde_json::json!`. Field names are part of the contract — the widget and the
Postman collection both depend on them.

```jsonc
// POST /ask
{
  "answer": "...",
  "sources": [ { "index": 1, "score": 0.83, "document_id": "...", "text": "..." } ],
  "conversation_id": "7045945d-3a0e-4b69-9749-326871ef7516"
}

// POST /search
{ "hits": [ { "score": 0.83, "text": "...", "document_id": "..." } ] }

// Any error
{ "error": "human readable message" }
```

`sources[].index` is 1-based and matches the `[1] [2] [3]` numbering fed to the LLM as CONTEXT. The
system prompt forbids the model from emitting those markers in its prose, so `index` is the only way
a client can map an answer back to a passage. Do not renumber it.

When retrieval finds nothing above `RAG_SCORE_THRESHOLD`, the response is still `200` with
`answer: NO_ANSWER` and `sources: []`. Not a 404 — "I don't know" is a successful answer.

---

## SSE contract (`POST /ask/stream`)

Named events, in order:

| Event | Payload | Count |
| --- | --- | --- |
| `conversation` | conversation UUID as a bare string | exactly 1, first |
| `sources` | JSON array, same shape as `/ask`'s `sources` | exactly 1 |
| `token` | a text fragment, raw | 0..n |
| `done` | empty | exactly 1, terminal |
| `error` | `{e:#}` message | 0..1, terminal |

The client must emit `conversation` into its state before the first `token`, because that id is how
the next turn continues the conversation. `error` and `done` are both terminal; a stream that ends
without either means the connection dropped.

---

## `conversation_id`: empty string means "new"

```rust
/// `""`, whitespace and `null` all mean "no conversation yet".
#[serde(default, deserialize_with = "empty_string_as_none")]
conversation_id: Option<uuid::Uuid>,
```

**Why this exists:** `#[serde(default)]` alone only covers an *absent* key. A present-but-empty
string is what an untouched form field or an unset Postman variable sends. Rejecting it would fail
the very first request of every conversation — precisely the one meant to create it. A non-empty
value that is not a UUID is still a real client bug: the custom deserializer raises, and Axum's
`Json` extractor turns that into `422 Unprocessable Entity` — not `400`, because the body *was*
valid JSON.

This behaviour is pinned by unit tests at the bottom of `handlers.rs`. If you change the
deserializer, those tests are the spec.

---

## Deprecated: `POST /documents` (multipart proxy)

This handler buffers the entire file in the API process's memory and republishes it onto the legacy
`ingest_jobs` queue. It exists only until clients migrate.

**Scheduled for deletion together with:** `crates/api/src/queue.rs`, and the worker's
`consume_legacy` / `LEGACY_QUEUE` (`crates/worker/src/main.rs`).

**Use instead:** `POST /documents/upload-url` → client `PUT`s straight to MinIO → MinIO publishes
`ObjectCreated` over AMQP → worker ingests. The API never sees the bytes. See
[rag-pipeline.md](rag-pipeline.md).

Do not add features to the deprecated path. Do not add new callers.

---

## Maintenance

Review when routes, response shapes or the SSE event set change — and at sprint planning. The
endpoint table above and `crates/api/src/main.rs` must agree; if you add a route, update both in the
same commit. `postman/bot_flow.postman_collection.json` is the third copy of this contract.

Related: [tenant-isolation.md](tenant-isolation.md), [security-policies.md](security-policies.md),
[code-conventions.md](code-conventions.md).
