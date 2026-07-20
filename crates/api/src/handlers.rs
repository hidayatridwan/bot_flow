use crate::auth::{self, Actor, AdminAuth, AuthTenant};
use crate::queue::{self, IngestJob};
use crate::rate_limit;
use anyhow::Context;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use qdrant_client::qdrant::{
    value::Kind, Condition, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder,
    DeletePointsBuilder, Distance, FieldType, Filter, KeywordIndexParamsBuilder, Modifier,
    QueryPointsBuilder, SparseVectorParamsBuilder, SparseVectorsConfigBuilder, VectorParamsBuilder,
};
use qdrant_client::Qdrant;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::conversation;
use crate::error::AppError;
use crate::state::AppState;
use crate::upload;
use common::embedding::EMBEDDING_DIM;
use common::key;
use sqlx::{PgConnection, PgExecutor, Row};

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, StreamExt};
use std::convert::Infallible;

// The collection name lives in `common` (versioned — see its doc). Defined once for the same
// reason the chunker and the embedding client are: the api searches what the worker wrote, and a
// disagreement here is a silently empty result set, not a build error.
use common::{sparse::SPARSE_VECTOR, COLLECTION};

// The context passages stay numbered: the numbering is what keeps the model anchored to a specific
// passage rather than blending them. It just must not surface those numbers to the reader.
// The language rule is about presentation, not content: it constrains what the answer is written in,
// never what it may be drawn from, so it loosens nothing in invariant 4 or 5. Without it the model
// picks a language on its own and picks inconsistently — the same English CV answered `siapa imam?`
// in English and a longer Indonesian question in Indonesian. Being answered in a language you did not
// ask in reads as a broken bot, and the tenant cannot fix it: this prompt is ours, not theirs.
const RAG_SYSTEM_PROMPT: &str = "You are a customer service assistant. Answer the user's question \
    ONLY using the numbered CONTEXT passages below. If the answer is not in the context, say \
    honestly that you don't have that information — do not make anything up. Be concise. \
    Write the answer as plain prose: never include citation markers, bracketed numbers, or any \
    reference to the passage numbers. \
    Always answer in the same language as the user's question, even when the passages are in a \
    different language — translate what you need from them rather than switching language.";

#[derive(serde::Serialize)]
struct Hit {
    score: f32,
    text: String,
    document_id: String,
}

/// Create the collection if it doesn't exist. Safe to call repeatedly. Called from main at startup.
pub async fn ensure_collection(qdrant: &Qdrant) -> anyhow::Result<()> {
    if qdrant.collection_exists(COLLECTION).await? {
        tracing::info!("collection '{COLLECTION}' already exists");
        return Ok(());
    }
    // `collection_exists` above and `create_collection` here are two calls, so two processes
    // starting together both see "absent" and both create. One wins; the loser must not die.
    // Two API instances booting simultaneously is normal, and so is a parallel test suite.
    let created = qdrant
        .create_collection(
            CreateCollectionBuilder::new(COLLECTION)
                .vectors_config(VectorParamsBuilder::new(EMBEDDING_DIM, Distance::Cosine))
                // D7: a sparse vector is WRITTEN by the worker from phase 10 and QUERIED by nobody
                // until 10b. Splitting write from query is what buys one migration and one variable
                // per measurement: phase 10's delta is attributable to chunking alone, and 10b's to
                // fusion alone, with no second re-index in between. `Modifier::Idf` makes Qdrant
                // compute IDF server-side at query time — a client-side IDF would need corpus
                // statistics that change on every ingest and would silently drift.
                .sparse_vectors_config(sparse_config()),
        )
        .await;
    if let Err(e) = created {
        if qdrant.collection_exists(COLLECTION).await? {
            tracing::info!("collection '{COLLECTION}' was created concurrently; continuing");
            return Ok(());
        }
        return Err(anyhow::Error::new(e).context("failed to create collection"));
    }

    // Index the tenant_id payload with the multitenancy optimization (is_tenant=true).
    // Created BEFORE ingest (here, when the collection is first born) so HNSW becomes filter-aware.
    qdrant
        .create_field_index(
            CreateFieldIndexCollectionBuilder::new(COLLECTION, "tenant_id", FieldType::Keyword)
                .field_index_params(KeywordIndexParamsBuilder::default().is_tenant(true)),
        )
        .await
        .context("failed to create tenant_id index")?;

    // D5: `document_id` is filtered by the worker's re-index delete AND by phase 8's deletion saga.
    // Unindexed, both were full scans of the collection.
    qdrant
        .create_field_index(CreateFieldIndexCollectionBuilder::new(
            COLLECTION,
            "document_id",
            FieldType::Keyword,
        ))
        .await
        .context("failed to create document_id index")?;

    tracing::info!(
        "collection '{COLLECTION}' created (dim={EMBEDDING_DIM}, cosine) + tenant_id/document_id \
         indexes + sparse '{SPARSE_VECTOR}' (written from phase 10, queried in 10b)"
    );
    Ok(())
}

/// The sparse leg's config: one named vector, with IDF computed server-side (D7).
fn sparse_config() -> SparseVectorsConfigBuilder {
    let mut cfg = SparseVectorsConfigBuilder::default();
    cfg.add_named_vector_params(
        SPARSE_VECTOR,
        SparseVectorParamsBuilder::default().modifier(Modifier::Idf),
    );
    cfg
}

/// The documents behind an answer, deduplicated. `/ingest`-era chunks carry an empty document_id
/// and are skipped: there is nothing to attribute them to, which is the debt phase 11 closed for
/// everything written since.
fn source_document_ids(hits: &[Hit]) -> Vec<String> {
    let mut ids: Vec<String> = hits
        .iter()
        .filter(|h| !h.document_id.is_empty())
        .map(|h| h.document_id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// MANDATORY filter: restrict the query to points owned by this tenant only.
/// The "mandatory filter at the app layer" leg of defense-in-depth.
fn tenant_filter(tenant_id: &str) -> Filter {
    Filter::must([Condition::matches("tenant_id", tenant_id.to_string())])
}

pub async fn health(State(state): State<AppState>) -> Json<Value> {
    // Probe every dependency concurrently: a slow one shouldn't serialize behind the others.
    let postgres = async { sqlx::query("SELECT 1").execute(&state.db).await.is_ok() };
    let qdrant = async { state.qdrant.health_check().await.is_ok() };
    let redis = async {
        let mut conn = state.redis.clone();
        matches!(redis::cmd("PING").query_async::<String>(&mut conn).await, Ok(pong) if pong == "PONG")
    };
    // HEAD on the bucket — proves credentials and reachability, not just a live socket.
    let minio = async { state.s3.exists().await.unwrap_or(false) };

    let (postgres_ok, qdrant_ok, redis_ok, minio_ok) = tokio::join!(postgres, qdrant, redis, minio);

    // lapin tracks the channel's liveness locally; a dropped connection flips this to false.
    let rabbitmq_ok = state.amqp.status().connected();

    let ok = postgres_ok && qdrant_ok && redis_ok && rabbitmq_ok && minio_ok;
    Json(json!({
        "status": if ok { "ok" } else { "degraded" },
        "postgres": postgres_ok,
        "qdrant": qdrant_ok,
        "redis": redis_ok,
        "rabbitmq": rabbitmq_ok,
        "minio": minio_ok,
    }))
}

/// `GET /metrics` — Prometheus exposition. Registered only when `METRICS_TOKEN` is set.
///
/// Counters come from the process; gauges are read fresh, because a cached gauge is a number that
/// can be wrong in a way nobody notices.
pub async fn metrics(
    _auth: crate::auth::MetricsAuth,
    State(state): State<AppState>,
) -> Result<([(axum::http::header::HeaderName, &'static str); 1], String), AppError> {
    use sqlx::Row;

    // Fleet-wide counts via the SECURITY DEFINER functions. A plain aggregate here would be scoped
    // by RLS to a tenant that is never set, return zero rows, and report success — see 0014.
    let mut gauges = crate::metrics::Gauges::default();
    for row in sqlx::query("SELECT status, n FROM metrics_document_counts()")
        .fetch_all(&state.db)
        .await?
    {
        gauges
            .documents
            .push((row.get::<String, _>("status"), row.get::<i64, _>("n")));
    }
    for row in sqlx::query("SELECT kind, n FROM metrics_overdue_counts()")
        .fetch_all(&state.db)
        .await?
    {
        gauges
            .overdue
            .push((row.get::<String, _>("kind"), row.get::<i64, _>("n")));
    }
    gauges.tenants = sqlx::query("SELECT count(*) AS n FROM tenants")
        .fetch_one(&state.db)
        .await?
        .get::<i64, _>("n");

    gauges.queues = queue_gauges(&state).await;

    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        crate::metrics::render(&state.metrics, &gauges),
    ))
}

/// Queue depth and consumer count, via a **throwaway channel**.
///
/// A passive declare of a queue that does not exist is a channel-level `NOT_FOUND`, and the broker
/// closes the channel. Doing this on `state.amqp` would kill the publishing channel on the first
/// scrape of a fresh deployment, and the only symptom anywhere would be `/health` reporting
/// rabbitmq down — the `lapin::Connection` trap, one layer up.
///
/// Returns **nothing** on failure rather than zeros: a `0` that means "I could not ask" is
/// indistinguishable from a dead worker, and would page someone on a broker hiccup. The alert rule
/// handles the missing case with `absent()`.
async fn queue_gauges(state: &AppState) -> Vec<(String, u64, u64)> {
    use lapin::options::QueueDeclareOptions;

    let Ok(channel) = state.amqp_conn.create_channel().await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for name in ["document_events", "document_events.dlq"] {
        let declared = channel
            .queue_declare(
                name,
                QueueDeclareOptions {
                    passive: true,
                    ..Default::default()
                },
                lapin::types::FieldTable::default(),
            )
            .await;
        match declared {
            Ok(q) => out.push((
                name.to_string(),
                q.message_count() as u64,
                q.consumer_count() as u64,
            )),
            // The channel is now closed by the broker; nothing further can be asked on it. That is
            // exactly why it is a throwaway.
            Err(_) => break,
        }
    }
    let _ = channel.close(200, "metrics scrape complete").await;
    out
}

#[derive(Deserialize)]
pub struct IngestRequest {
    /// Required: this creates a *document*, and a document has a name. The extension also decides
    /// how the sidecar will parse it, so it is not decoration.
    filename: String,
    text: String,
    /// The caller's own id for this content — a CMS page id, a row key. Optional; absent means
    /// "always create a new document", which is what uploading twice does.
    external_id: Option<String>,
}

/// `POST /ingest` — index text the caller hands us inline, as a real document.
///
/// **This used to write vectors and nothing else**: random point ids, a payload of `text` +
/// `tenant_id`, and no `documents` row anywhere. Those points could never be listed, re-indexed or
/// removed — CLAUDE.md called it the largest single piece of debt in the system, and a processor
/// that cannot erase a named document has a compliance problem, not a backlog item.
///
/// **The fix is not a second ingestion path — it is the absence of one.** The bytes are written to
/// MinIO under a normal object key, and everything downstream is the machinery uploads already use:
/// storage announces the object, the worker claims it, chunks it with `common::chunk`, and writes
/// full provenance. So this route inherits the lifecycle, the deletion saga, the reaper and the
/// re-index driver for free, and there is exactly one recipe for turning text into vectors.
///
/// That last part is why the obvious design — embed and upsert right here, synchronously — is
/// wrong. It would store the text *only* as vectors, and `worker reindex` walks `documents` rows
/// reading `object_key`: on the next collection version bump every inline document would silently
/// vanish. Unaccountable data is the thing this route is being fixed for.
///
/// The cost is that it is now **asynchronous**: `202`, then poll `GET /documents` until `ready`.
pub async fn ingest(
    State(state): State<AppState>,
    tenant: AuthTenant,             // FromRequestParts — reads headers, no body
    Json(req): Json<IngestRequest>, // FromRequest — consumes body, MUST be last
) -> Result<(StatusCode, Json<Value>), AppError> {
    tenant.require_secret()?;
    // Still a billed pipeline per call, exactly as before — the embedding just happens in the
    // worker now rather than here.
    rate_limit::check(&state, &tenant.tenant_id).await?;

    // Same validator as the presigned path, so the two cannot disagree about what a document is.
    let ext = upload::checked_extension(&req.filename)?;

    if req.text.trim().is_empty() {
        return Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            "text must not be empty",
        ));
    }
    // **The one place invariant 11 does not bind.** "Upload size cannot be enforced at upload time"
    // is a fact about presigned signatures, which cover method, key and expiry but never body
    // length. Here the bytes are in the request, so there *is* an earlier — and refusing now beats
    // storing them and quarantining later.
    if req.text.len() > state.max_upload_bytes {
        return Err(AppError::client(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("text exceeds the {}-byte limit", state.max_upload_bytes),
        ));
    }

    // Row first, object second — the same order as the presigned path, for the same reason. A row
    // with no object is the abandoned case the reaper settles; an object with no row is an orphan
    // nothing in the system can find, which is the bug being fixed.
    let (document_id, object_key) = upload::inline_document(
        &state.db,
        &tenant.tenant_id,
        &req.filename,
        &ext,
        req.external_id.as_deref(),
    )
    .await?;

    // Content type from the extension we validated, never from the client (see Security).
    state
        .s3
        .put_object_with_content_type(
            &object_key,
            req.text.as_bytes(),
            key::content_type_for(&ext),
        )
        .await
        .map_err(|e| {
            AppError::Internal(anyhow::Error::new(e).context("failed to store inline document"))
        })?;

    // 202, not 201: the document exists, but it is not searchable until the worker has indexed it.
    // Saying `ready` here would be a lie a caller would act on.
    Ok((
        StatusCode::ACCEPTED,
        json!({
            "document_id": document_id.to_string(),
            "status": "uploading",
            "note": "indexing is asynchronous; poll GET /documents until status is `ready`",
        })
        .into(),
    ))
}

#[derive(Deserialize)]
pub struct SearchRequest {
    query: String,
    #[serde(default = "default_limit")]
    limit: u64,
}

fn default_limit() -> u64 {
    3
}

/// The most passages a caller may ask for. `limit` came straight off the request body as a `u64`
/// with no ceiling, so `{"limit": 100000}` was a valid request — one embedding call and a Qdrant
/// scan sized by a stranger.
const MAX_LIMIT: u64 = 20;

/// How much deeper than `limit` to search before applying the relevance floor.
///
/// **The floor used to be applied *after* the limit, which made it shrink the answer instead of
/// improving it.** Ask for 3, have one hit fall below the floor, get 2 — while a perfectly good
/// fourth sat one rank down, already retrieved and thrown away. Over-fetching means the floor
/// removes weak passages and the strong ones behind them move up, which is what a relevance floor
/// is supposed to do.
fn over_fetch(limit: u64) -> u64 {
    (limit * 4).max(limit + 8)
}

/// `limit` is the caller's, bounded. Anything above the cap is the caller's mistake, not ours.
fn checked_limit(limit: u64) -> Result<u64, AppError> {
    if limit == 0 || limit > MAX_LIMIT {
        return Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            "limit must be between 1 and 20",
        ));
    }
    Ok(limit)
}

/// Query string of `GET /documents`. Both fields are optional, which is what keeps an existing
/// `sk_` client working unchanged — it simply starts receiving a bounded first page.
#[derive(Deserialize)]
pub struct ListDocumentsQuery {
    limit: Option<u64>,
    /// The `next_cursor` from a previous page. Opaque to the caller by contract, even though it is
    /// legible: nothing but a token we minted is accepted.
    before: Option<String>,
}

/// Rows returned when the caller names no `limit`.
///
/// Chosen to be larger than any tenant's screen and smaller than any tenant's table. It is also the
/// value an **un-updated client** silently receives, which is the entire point of defaulting rather
/// than requiring the parameter: the unbounded read closes for every caller on deploy, not just for
/// callers who adopt the parameter.
const DEFAULT_PAGE_LIMIT: u64 = 50;

/// The most rows one page may carry. A ceiling on `?limit=`, for the same reason [`MAX_LIMIT`]
/// bounds `/search`: without it `?limit=100000` is a valid request that reinstates exactly the
/// unbounded read this parameter exists to close.
const MAX_PAGE_LIMIT: u64 = 200;

fn checked_page_limit(limit: Option<u64>) -> Result<u64, AppError> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    if limit == 0 || limit > MAX_PAGE_LIMIT {
        return Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            "limit must be between 1 and 200",
        ));
    }
    Ok(limit)
}

/// A keyset cursor: the `(created_at, id)` of the last row of a page.
///
/// **Keyset rather than `OFFSET` because this listing is polled.** With an offset, a document
/// created between two polls shifts every following row by one, so the reader sees a row twice or
/// misses one entirely — and neither shows up as an error. A cursor names a *position in the
/// ordering*, so an insert above it changes nothing below it.
///
/// Both halves are required. `created_at` alone is not unique (see migration 0016), and a cursor on
/// a non-unique key loses exactly the rows that share the boundary timestamp.
struct Cursor {
    created_at: String,
    id: uuid::Uuid,
}

/// Render `(created_at, id)` as the `next_cursor` string.
///
/// The `T` is not cosmetic: Postgres renders `timestamptz::text` with a **space** separator
/// (`2026-07-20 04:09:35.353682+00`), and a space in a query parameter is the `%20`-or-`+`
/// ambiguity that `event.rs` already documents for MinIO keys. Emitting ISO-8601 keeps the token
/// safe to paste into a URL by hand, and Postgres accepts the `T` form back verbatim.
fn encode_cursor(created_at: &str, id: uuid::Uuid) -> String {
    format!("{}~{}", created_at.replacen(' ', "T", 1), id)
}

/// Parse a cursor, rejecting anything we did not mint.
///
/// The strictness is load-bearing rather than fussy. The timestamp half is interpolated into the
/// query as a `::timestamptz` bind, and a value Postgres cannot cast raises a database error, which
/// the blanket `From` turns into a **500** — an internal error for what is plainly a caller's
/// malformed input. Validating the shape here is what keeps that a `422`, and it is why this
/// accepts only the exact format [`encode_cursor`] emits instead of anything date-like.
fn parse_cursor(raw: &str) -> Result<Cursor, AppError> {
    let invalid = || AppError::client(StatusCode::UNPROCESSABLE_ENTITY, "invalid cursor");

    // `rsplit_once`: a UUID contains no `~`, so the last one is always the separator.
    let (ts, id) = raw.rsplit_once('~').ok_or_else(invalid)?;
    let id = uuid::Uuid::parse_str(id).map_err(|_| invalid())?;
    if !is_our_timestamp(ts) {
        return Err(invalid());
    }
    Ok(Cursor {
        created_at: ts.to_string(),
        id,
    })
}

/// Does this string match the timestamp shape we emit — `YYYY-MM-DDTHH:MM:SS[.ffffff]+00[:00]`?
///
/// Deliberately a shape *and range* check, not merely a character-class one. `9999-99-99T99:99:99`
/// passes any plausible "looks like a date" test and still fails the `::timestamptz` cast, which is
/// the 500 this function exists to prevent.
fn is_our_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    // `2026-07-20T04:09:35` is the shortest accepted form.
    if b.len() < 19 {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let num = |r: std::ops::Range<usize>| s[r].parse::<u32>().unwrap_or(u32::MAX);

    if !(digits(0..4) && b[4] == b'-' && digits(5..7) && b[7] == b'-' && digits(8..10)) {
        return false;
    }
    if !(b[10] == b'T'
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19))
    {
        return false;
    }
    if !(1..=12).contains(&num(5..7))
        || !(1..=31).contains(&num(8..10))
        || num(11..13) > 23
        || num(14..16) > 59
        // 60 is a leap second, which Postgres accepts.
        || num(17..19) > 60
    {
        return false;
    }

    // Optional fractional seconds, then an offset we must recognise.
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let n = frac.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 || n > 6 {
            return false;
        }
        rest = &frac[n..];
    }
    matches!(rest, "" | "Z" | "+00" | "+00:00" | "-00" | "-00:00")
}

pub async fn search(
    State(state): State<AppState>,
    actor: Actor, // FromRequestParts — reads headers, no body
    Json(req): Json<SearchRequest>,
) -> Result<Json<Value>, AppError> {
    // Raw retrieval is not "asking a question", so a `pk_` is refused here even though it may ask
    // freely (invariants 15 and 27 are not in tension — they draw the same line from both sides).
    // Not a confidentiality gate: `/ask` already returns `sources[].text` to a `pk_`. It bounds spend.
    actor.require_management()?;
    let limit = checked_limit(req.limit)?;
    rate_limit::check(&state, &actor.tenant_id).await?;

    let vector = state.embedder.embed_one(&req.query).await?;

    let response = state
        .qdrant
        .query(
            QueryPointsBuilder::new(COLLECTION)
                .query(vector)
                .limit(limit)
                .filter(tenant_filter(&actor.tenant_id))
                .with_payload(true),
        )
        .await?;

    let hits: Vec<Hit> = response
        .result
        .into_iter()
        .filter_map(|p| {
            let text = p
                .payload
                .get("text")
                .and_then(|v| v.kind.as_ref())
                .and_then(|k| match k {
                    Kind::StringValue(s) => Some(s.clone()),
                    _ => None,
                })?;
            let document_id = p
                .payload
                .get("document_id")
                .and_then(|v| v.kind.as_ref())
                .and_then(|k| match k {
                    Kind::StringValue(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            Some(Hit {
                score: p.score,
                text,
                document_id,
            })
        })
        .collect();

    // D11: `/search` deliberately does NOT apply the floor — it is the instrument README tells you
    // to use to *choose* the floor, and applying it there destroys the only way to see what sits
    // just below. Returning the threshold makes that divergence legible instead of surprising: a
    // caller can see exactly which of these hits `/ask` would have dropped.
    Ok(Json(json!({
        "hits": hits,
        "rag_score_threshold": state.rag_score_threshold,
        "note": "raw scores; /ask drops hits below rag_score_threshold",
    })))
}

/// Record the outcome of one question.
///
/// **One helper, two call sites, deliberately.** `ask` and `ask_stream` each implement invariant 4's
/// refusal branch independently; two bare `fetch_add`s would be exactly the drift this repo keeps
/// warning about, and the symptom would be a refusal rate that is quietly half the real one.
fn record_ask(state: &AppState, sources: usize) {
    crate::metrics::Metrics::incr(&state.metrics.ask_total);
    if sources == 0 {
        crate::metrics::Metrics::incr(&state.metrics.ask_refused_total);
    } else {
        crate::metrics::Metrics::add(&state.metrics.ask_sources_total, sources as u64);
    }
}

const NO_ANSWER: &str = "Sorry, I couldn't find any relevant information.";

/// What an LLM failure looks like to whoever is chatting.
///
/// Invariant 16, hand-rolled. An SSE frame is yielded mid-stream and never passes through
/// `AppError::into_response`, so it is the one client-facing surface that does not inherit the
/// log-in-full / answer-generically split that `?` gives every ordinary handler for free.
///
/// The detail this replaces was `{e:#}`, which for an LLM error is `llm.rs`'s
/// `LLM replied {status}: {body}` — **a body we neither author nor control**. A real 401 from the
/// gateway was observed carrying a key fragment, the key's full SHA-256 hash, and the name of an
/// internal table. That is one gateway's choice on one day; the next one's is not ours to assume,
/// which is the actual reason this cannot be a judgement call about how bad the body looks.
///
/// On a `pk_` widget this frame reaches a stranger's browser (invariant 15) — the least private
/// surface in the system.
const STREAM_FAILED: &str = "the answer could not be completed";

/// The maximum wall-clock life of one streamed answer, and the close of invariant 28's residue.
///
/// **Read this before moving it, and especially before putting it back on the HTTP client.**
/// `READ_TIMEOUT` bounds *silence between reads*, which is what a hung gateway looks like — it says
/// nothing about total duration, so a gateway emitting one token every 59 seconds streams forever
/// while never once looking unhealthy. That is the hole this closes, and it can only be closed
/// here: a `.timeout()` on the reqwest client is a total deadline **including the body**, and this
/// body *is* the answer, so it would cap how long an answer may be on every call (the trap table's
/// own entry, and `llm.rs` has two tests that go red if someone adds it).
///
/// **The deadline is not a failure, and that distinction is the whole design.** The obvious
/// implementation sets `failed = true`, which is wrong twice over: the client gets an `error` frame
/// after having already rendered three good paragraphs, and `append_turn` is then skipped, so
/// invariant 7 drops from history a turn that only our own ceiling truncated — the user loses the
/// answer they watched arrive *and* the record of having asked. So this path emits a normal `done`
/// and **persists what arrived**.
///
/// The cost of persisting, stated because it is real: an answer cut mid-sentence becomes history
/// the next rewrite reasons over. That is the better half of the trade — a slightly awkward
/// follow-up beats a vanished conversation — but it is a trade, not a free win.
///
/// Sized so that firing is evidence of a misbehaving gateway rather than a long answer: `MAX_TOKENS`
/// is 4096, which a healthy gateway streams in well under a minute. A named constant beside the code
/// it bounds, not an env var, for the reason `MAX_TOKENS` and `READ_TIMEOUT` are: a bound on our own
/// resource use is a correctness decision, not a deployment preference.
const STREAM_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

/// `""`, whitespace and `null` all mean "no conversation yet".
///
/// `#[serde(default)]` alone only covers an ABSENT key. A present-but-empty string is what an
/// untouched form field or Postman variable sends, and rejecting it 400s the very first request of
/// every conversation — precisely the one that is meant to create it.
fn empty_string_as_none<'de, D>(d: D) -> Result<Option<uuid::Uuid>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(d)?;
    match raw.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        // A non-empty value that isn't a UUID is a real client bug — surface it.
        Some(s) => s.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

#[derive(Deserialize)]
pub struct AskRequest {
    query: String,
    #[serde(default = "default_limit")]
    limit: u64,
    /// Omit to start a new conversation; the id comes back in the response. An empty string or
    /// `null` is treated the same as omitting it.
    #[serde(default, deserialize_with = "empty_string_as_none")]
    conversation_id: Option<uuid::Uuid>,
}

/// Resolve the conversation, then rewrite the query against its history so pronouns and
/// implicit references become explicit before they are ever embedded.
///
/// Nothing is written here. The turn is persisted only once an answer exists, so a failed
/// request leaves no trace for the next rewrite to trip over.
async fn prepare(
    state: &AppState,
    tenant_id: &str,
    req: &AskRequest,
) -> Result<(uuid::Uuid, String), AppError> {
    let conversation_id = conversation::ensure(&state.db, tenant_id, req.conversation_id).await?;
    let history = conversation::recent(
        &state.db,
        tenant_id,
        conversation_id,
        conversation::HISTORY_LIMIT,
    )
    .await?;
    // The user's own words go into history; the rewritten form is only a retrieval/answer key.
    let standalone = conversation::rewrite(&state.llm, &history, &req.query).await;
    Ok((conversation_id, standalone))
}

/// Answer a question from the tenant's own documents.
///
/// `Actor`, and no gate: a `pk_` must reach this (it is the only thing a publishable key may do, and
/// that limit is what makes it safe to print), so the two stronger credentials are admitted to a route
/// the weakest already reaches. Adding `require_management()` here would 403 every widget — see
/// invariant 27.
pub async fn ask(
    State(state): State<AppState>,
    actor: Actor,
    Json(req): Json<AskRequest>,
) -> Result<Json<Value>, AppError> {
    // Keyed on the tenant, not the credential, so a dashboard question and a widget question draw on
    // one bucket. That is what bounds the spend this route's openness would otherwise invite.
    let limit = checked_limit(req.limit)?;
    rate_limit::check(&state, &actor.tenant_id).await?;

    let (conversation_id, standalone) = prepare(&state, &actor.tenant_id, &req).await?;

    let relevant = retrieve(&state, &actor.tenant_id, &standalone, limit).await?;
    record_ask(&state, relevant.len());

    if relevant.is_empty() {
        conversation::append_turn(
            &state.db,
            &actor.tenant_id,
            conversation_id,
            &req.query,
            NO_ANSWER,
            &[], // a refusal cites nothing — there was no context (invariant 4)
        )
        .await?;
        return Ok(Json(json!({
            "answer": NO_ANSWER,
            "sources": [],
            "conversation_id": conversation_id.to_string(),
        })));
    }

    let context = relevant
        .iter()
        .enumerate()
        .map(|(i, h)| format!("[{}] {}", i + 1, h.text))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!("CONTEXT:\n{context}\n\nQUESTION: {standalone}");

    let answer = match state.llm.answer(RAG_SYSTEM_PROMPT, &user).await {
        Ok(a) => {
            crate::metrics::Metrics::incr(&state.metrics.llm_ok);
            a
        }
        Err(e) => {
            crate::metrics::Metrics::incr(&state.metrics.llm_error);
            return Err(e.into());
        }
    };

    conversation::append_turn(
        &state.db,
        &actor.tenant_id,
        conversation_id,
        &req.query,
        &answer,
        &source_document_ids(&relevant),
    )
    .await?;

    let sources: Vec<Value> = relevant
        .iter()
        .enumerate()
        .map(|(i, h)| json!({ "index": i + 1, "score": h.score, "document_id": h.document_id, "text": h.text }))
        .collect();

    Ok(Json(
        json!({ "answer": answer, "sources": sources, "conversation_id": conversation_id.to_string() }),
    ))
}

#[derive(Deserialize)]
pub struct UploadUrlRequest {
    filename: String,
}

/// Mint a presigned PUT. The bytes go straight from the client to MinIO; this process never
/// sees them, which is the entire point of the endpoint.
pub async fn create_upload_url(
    State(state): State<AppState>,
    actor: Actor,
    Json(req): Json<UploadUrlRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    actor.require_management()?;
    // Keyed on the tenant, not the credential: spend is per tenant, so a dashboard upload and an
    // `sk_` upload draw on the same bucket.
    rate_limit::check(&state, &actor.tenant_id).await?;

    let session = upload::create_session(
        &state.db,
        &state.s3_public,
        &actor.tenant_id,
        &req.filename,
        state.presign_ttl_secs,
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "document_id": session.document_id.to_string(),
            "upload_url": session.upload_url,
            "method": "PUT",
            "expires_at": session.expires_at,
        })),
    ))
}

/// Re-mint a URL whose TTL lapsed before the client finished uploading.
pub async fn refresh_upload_url(
    State(state): State<AppState>,
    actor: Actor,
    Path(document_id): Path<uuid::Uuid>,
) -> Result<Json<Value>, AppError> {
    actor.require_management()?;
    rate_limit::check(&state, &actor.tenant_id).await?;

    let session = upload::refresh_session(
        &state.db,
        &state.s3_public,
        &actor.tenant_id,
        document_id,
        state.presign_ttl_secs,
    )
    .await?;

    Ok(Json(json!({
        "document_id": session.document_id.to_string(),
        "upload_url": session.upload_url,
        "method": "PUT",
        "expires_at": session.expires_at,
    })))
}

/// DEPRECATED: proxies the whole file through this process, buffered in memory. Superseded by
/// `create_upload_url`. Retained only so existing clients keep working during the rollout.
pub async fn upload_document(
    State(state): State<AppState>,
    tenant: AuthTenant,
    mut multipart: Multipart, // FromRequest (consumes body) => must be last
) -> Result<(StatusCode, Json<Value>), AppError> {
    tenant.require_secret()?;
    rate_limit::check(&state, &tenant.tenant_id).await?;

    // Pull the "file" field out of the multipart form.
    let mut filename = None;
    let mut data = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::client(StatusCode::BAD_REQUEST, format!("invalid multipart: {e}")))?
    {
        if field.name() == Some("file") {
            filename = field.file_name().map(|s| s.to_string());
            data = Some(field.bytes().await.map_err(|e| {
                AppError::client(StatusCode::BAD_REQUEST, format!("read failed: {e}"))
            })?);
        }
    }

    let filename = filename
        .ok_or_else(|| AppError::client(StatusCode::BAD_REQUEST, "missing 'file' field"))?;
    let data =
        data.ok_or_else(|| AppError::client(StatusCode::BAD_REQUEST, "missing 'file' field"))?;

    let document_id = uuid::Uuid::new_v4();
    // Tenant-prefixed key keeps each tenant's objects partitioned in storage too.
    let object_key = format!("{}/{}", tenant.tenant_id, document_id);

    // 1. Store raw bytes in MinIO. rust-s3 returns Ok even on non-2xx, so check the status.
    let resp = state.s3.put_object(&object_key, &data).await?;
    if resp.status_code() != 200 {
        return Err(AppError::client(
            StatusCode::BAD_GATEWAY,
            format!("S3 upload failed (status {})", resp.status_code()),
        ));
    }

    // 2. Record the document (status defaults to 'pending').
    let mut tx = crate::db::tenant_tx(&state.db, &tenant.tenant_id).await?;
    sqlx::query(
        "INSERT INTO documents (id, tenant_id, filename, object_key) VALUES ($1, $2, $3, $4)",
    )
    .bind(document_id)
    .bind(&tenant.tenant_id)
    .bind(&filename)
    .bind(&object_key)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // 3. Enqueue background processing.
    queue::publish_ingest_job(
        &state.amqp,
        &IngestJob {
            document_id: document_id.to_string(),
            tenant_id: tenant.tenant_id.clone(),
            object_key,
            filename,
        },
    )
    .await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "document_id": document_id.to_string(), "status": "pending" })),
    ))
}

/// Embed the query, search this tenant's vectors, and keep only hits at/above the threshold.
async fn retrieve(
    state: &AppState,
    tenant_id: &str,
    query: &str,
    limit: u64,
) -> Result<Vec<Hit>, AppError> {
    // The ask/search path's own embedding calls. A DIFFERENT signal from the worker's: this is the
    // gateway hurting questions, not ingestion. `is_fatal` is the worker's own retry classification,
    // reused so the two cannot disagree about what "fatal" means.
    let vector = match state.embedder.embed_one(query).await {
        Ok(v) => {
            crate::metrics::Metrics::incr(&state.metrics.embed_ok);
            v
        }
        Err(e) => {
            crate::metrics::Metrics::incr(if e.is_fatal() {
                &state.metrics.embed_fatal
            } else {
                &state.metrics.embed_retryable
            });
            return Err(e.into());
        }
    };

    let response = state
        .qdrant
        .query(
            QueryPointsBuilder::new(COLLECTION)
                .query(vector)
                // Deeper than asked for: the floor below removes weak hits, and without the extra
                // depth it would shrink the context rather than sharpen it.
                .limit(over_fetch(limit))
                .filter(tenant_filter(tenant_id))
                .with_payload(true),
        )
        .await?;

    let hits: Vec<Hit> = response
        .result
        .into_iter()
        .filter_map(|p| {
            let text = p
                .payload
                .get("text")
                .and_then(|v| v.kind.as_ref())
                .and_then(|k| match k {
                    Kind::StringValue(s) => Some(s.clone()),
                    _ => None,
                })?;
            let document_id = p
                .payload
                .get("document_id")
                .and_then(|v| v.kind.as_ref())
                .and_then(|k| match k {
                    Kind::StringValue(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            Some(Hit {
                score: p.score,
                text,
                document_id,
            })
        })
        .collect();

    tracing::info!(
        "retrieval scores: {:?} (threshold {})",
        hits.iter().map(|h| h.score).collect::<Vec<_>>(),
        state.rag_score_threshold
    );

    // Floor first, THEN truncate to what the caller asked for. The order is the whole fix.
    Ok(hits
        .into_iter()
        .filter(|h| h.score >= state.rag_score_threshold)
        .take(limit as usize)
        .collect())
}

/// The streaming twin of [`ask`]. Same principal, same absence of a gate, for the same reason
/// (invariant 27) — the pair must not diverge on who may call them.
pub async fn ask_stream(
    State(state): State<AppState>,
    actor: Actor,
    Json(req): Json<AskRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    let limit = checked_limit(req.limit)?;
    rate_limit::check(&state, &actor.tenant_id).await?;

    // Memory + rewrite before retrieval, so failures here are normal HTTP errors, not mid-stream.
    let (conversation_id, standalone) = prepare(&state, &actor.tenant_id, &req).await?;

    // Retrieval happens before streaming, so retrieval errors are normal HTTP errors (not mid-stream).
    let relevant = retrieve(&state, &actor.tenant_id, &standalone, limit).await?;
    record_ask(&state, relevant.len());

    // Captured for the turn record: which documents the model was shown. Computed here because
    // `relevant` is consumed building the SSE payload below.
    let source_docs = source_document_ids(&relevant);

    // We know the sources up front — send them as the first SSE event.
    let sources = json!(relevant
        .iter()
        .enumerate()
        .map(|(i, h)| json!({ "index": i + 1, "score": h.score, "document_id": h.document_id, "text": h.text }))
        .collect::<Vec<_>>());

    let context = relevant
        .iter()
        .enumerate()
        .map(|(i, h)| format!("[{}] {}", i + 1, h.text))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!("CONTEXT:\n{context}\n\nQUESTION: {standalone}");
    let empty = relevant.is_empty();
    let llm = state.llm.clone(); // move an owned client into the stream
    let db = state.db.clone();
    let tenant_id = actor.tenant_id.clone();
    let question = req.query.clone();

    let sse = async_stream::stream! {
        // 1. the conversation id, so a brand-new conversation can be continued by the client.
        yield Ok::<_, Infallible>(Event::default().event("conversation").data(conversation_id.to_string()));

        // 2. sources next.
        yield Ok(Event::default().event("sources").json_data(&sources).unwrap());

        if empty {
            // Persisted so history stays a faithful record of what the user was told.
            if let Err(e) = conversation::append_turn(&db, &tenant_id, conversation_id, &question, NO_ANSWER, &[]).await {
                tracing::error!("failed to persist turn: {e:?}");
            }
            yield Ok(Event::default().event("token").data(NO_ANSWER));
            yield Ok(Event::default().event("done").data(""));
            return;
        }

        // 3. stream the answer tokens, accumulating them so the full reply can be stored.
        let mut answer = String::new();
        let mut failed = false;
        // The wall clock. Absolute, taken once: a per-token deadline would reset on every delta and
        // bound nothing, which is precisely how a gateway trickling one token just inside
        // READ_TIMEOUT streamed forever.
        let deadline = tokio::time::Instant::now() + STREAM_DEADLINE;
        match llm.answer_stream(RAG_SYSTEM_PROMPT, &user).await {
            Ok(token_stream) => {
                futures_util::pin_mut!(token_stream); // make it pollable with .next()
                loop {
                    match tokio::time::timeout_at(deadline, token_stream.next()).await {
                        // The ceiling fired. Deliberately NOT `failed`: see STREAM_DEADLINE.
                        Err(_elapsed) => {
                            tracing::warn!(
                                chars = answer.len(),
                                "ask stream hit its duration ceiling; ending the answer early"
                            );
                            break;
                        }
                        Ok(None) => break, // the gateway finished
                        Ok(Some(item)) => match item {
                            Ok(text) if !text.is_empty() => {
                                answer.push_str(&text);
                                yield Ok(Event::default().event("token").data(text));
                            }
                            Ok(_) => {} // empty delta (role-only / [DONE]) — skip
                            Err(e) => {
                                failed = true;
                                tracing::error!("llm stream failed mid-answer: {e:#}");
                                yield Ok(Event::default().event("error").data(STREAM_FAILED));
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                failed = true;
                tracing::error!("llm stream failed to start: {e:#}");
                yield Ok(Event::default().event("error").data(STREAM_FAILED));
            }
        }

        // 4. persist the turn. A stream that *errored* may have emitted a partial answer —
        // storing it would make the next rewrite reason over a truncated sentence. A stream that
        // hit the deadline is the other case and is stored: see STREAM_DEADLINE for why the two
        // are not the same thing.
        if !failed && !answer.is_empty() {
            if let Err(e) = conversation::append_turn(&db, &tenant_id, conversation_id, &question, &answer, &source_docs).await {
                tracing::error!("failed to persist turn: {e:?}");
            }
        }

        // 5. done sentinel.
        yield Ok(Event::default().event("done").data(""));
    };

    Ok(Sse::new(sse).keep_alive(KeepAlive::default()))
}

pub async fn list_documents(
    State(state): State<AppState>,
    actor: Actor,
    Query(q): Query<ListDocumentsQuery>,
) -> Result<Json<Value>, AppError> {
    actor.require_management()?;
    let limit = checked_page_limit(q.limit)?;
    let cursor = q.before.as_deref().map(parse_cursor).transpose()?;
    // The dashboard reads this with a `sess_`; a tenant's own server reads it with an `sk_`. Both
    // yield the same tenant_id, so RLS below is identical either way — it is keyed on the string,
    // not on how the string was obtained.
    let mut tx = crate::db::tenant_tx(&state.db, &actor.tenant_id).await?;
    // Note: NO `WHERE tenant_id` — RLS scopes this to the current tenant automatically.
    // That's the whole point: forgetting the filter can't leak other tenants' rows.
    // Exclude `deleting`: a tombstoned document is on its way out (phase 8) and must not appear in
    // the tenant's list — it has already left, as far as they are concerned.
    // `failure_reason`, never `error`. They are two halves of one failure and only one of them may
    // leave the building: `error` holds raw parser stderr and our own post-mortems (invariant 16),
    // while `failure_reason` is a closed enum whose whole purpose is to be shown. Selecting `error`
    // here — even "just for debugging" — is how a gateway's internals reach a tenant's browser.
    //
    // The row-value comparison `(created_at, id) < ($1, $2)` is the keyset predicate, and it is one
    // expression rather than the `created_at < $1 OR (created_at = $1 AND id < $2)` it expands to
    // precisely so it cannot be written subtly wrong. It also matches the 0016 index directly.
    //
    // `limit + 1`: we ask for one row more than we return, and its existence is how we know whether
    // there is a next page. Counting the table instead would reinstate the full scan this is
    // replacing, one query along.
    //
    // **`ORDER BY documents.created_at` is qualified deliberately — do not "tidy" it to
    // `created_at`.** `created_at::text AS created_at` puts an output column of that name in scope,
    // and Postgres resolves a *bare* name in ORDER BY against the output list first, so the
    // unqualified form sorts by the **text rendering** rather than the timestamp.
    //
    // That costs the index. Measured on 5k rows: bare gave `Seq Scan` + `Sort Key:
    // ((created_at)::text)` at cost 371 for the first page; qualified gives an `Index Scan using
    // idx_documents_tenant_created` with no sort node at all, at cost 0.29. The 0016 index exists
    // for this query and the bare form never touches it.
    //
    // It reads like a correctness bug too — the keyset `WHERE` compares `timestamptz` while the
    // bare ORDER BY compares text — but it is not, and the reason is worth writing down so nobody
    // "fixes" it twice: `timestamptz` normalises to UTC on storage and renders in the session's
    // TimeZone, so every row's text carries the *same* offset and lexicographic order agrees with
    // chronological order. The bug is the plan, not the result.
    let rows = sqlx::query(
        "SELECT id, filename, status, failure_reason, created_at::text AS created_at
           FROM documents
          WHERE status <> 'deleting'
            AND ($1::text IS NULL OR (created_at, id) < ($1::timestamptz, $2::uuid))
          ORDER BY documents.created_at DESC, documents.id DESC
          LIMIT $3",
    )
    .bind(cursor.as_ref().map(|c| c.created_at.as_str()))
    .bind(cursor.as_ref().map(|c| c.id))
    .bind((limit + 1) as i64)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    // The probe row is evidence, not content — it must never be rendered.
    let has_more = rows.len() as u64 > limit;
    let page = &rows[..rows.len().min(limit as usize)];

    let docs: Vec<Value> = page
        .iter()
        .map(|r| {
            let id: uuid::Uuid = r.get("id");
            json!({
                "id": id.to_string(),
                "filename": r.get::<String, _>("filename"),
                "status": r.get::<String, _>("status"),
                // Null for a document that failed before phase 14 classified failures, and for
                // every document that has not failed at all. The client renders null as the old
                // both-causes copy rather than guessing.
                "failure_reason": r.get::<Option<String>, _>("failure_reason"),
                "created_at": r.get::<String, _>("created_at"),
            })
        })
        .collect();

    // Null rather than absent when the page is last, so a client can branch on one thing. Built
    // from the last **returned** row, never the probe.
    let next_cursor = has_more.then(|| page.last()).flatten().map(|r| {
        encode_cursor(
            &r.get::<String, _>("created_at"),
            r.get::<uuid::Uuid, _>("id"),
        )
    });

    Ok(Json(
        json!({ "documents": docs, "next_cursor": next_cursor, "limit": limit }),
    ))
}

/// `DELETE /documents/{id}` — erase a document across all three stores (phase 8).
///
/// A tombstone-guarded saga, because there is no transaction spanning Postgres, Qdrant and MinIO.
/// The row is moved to `deleting` first, on its own, under a row lock: that drops it from the
/// tenant's list immediately and — for a document a worker is still indexing — fences the worker out
/// (invariant 10). Everything after the tombstone is idempotent cleanup a crash can resume, which is
/// why the tombstone is committed before any store is touched.
///
/// Two outcomes:
/// - **`processing`** → `202`. A worker may still be writing vectors; deleting them now would race
///   its upsert and lose. The reaper sweep (a later commit) finishes it once the worker has provably
///   released. Until then the row is `deleting`: gone from listings, fenced.
/// - **anything else** (`ready`/`failed`/`expired`/`uploading`/`quarantined`) → `204`, done inline.
pub async fn delete_document(
    State(state): State<AppState>,
    actor: Actor,
    Path(document_id): Path<uuid::Uuid>,
) -> Result<StatusCode, AppError> {
    actor.require_management()?;

    // Opened before anything is touched, closed when the stores are clear. A row left open is an
    // erasure that started and did not finish — the case worth being able to find.
    let audit_id = crate::erasure::begin(
        &state.db,
        &actor.tenant_id,
        "document",
        Some(document_id),
        crate::erasure::actor_label(&actor.kind),
    )
    .await?;

    // Lock the row and read what we need before changing anything. Unknown id and another tenant's
    // id both arrive as `None` — RLS hides the latter — and both must 404: a 403 for one would make
    // the endpoint an oracle for which ids exist (invariants 8, 26). This SELECT-under-RLS, not a
    // blind DELETE, is also what dodges the corollary trap: a cross-tenant DELETE matches zero rows
    // and reports success, which would have us report a deletion that never happened.
    let mut tx = crate::db::tenant_tx(&state.db, &actor.tenant_id).await?;
    let row = sqlx::query("SELECT status, object_key FROM documents WHERE id = $1 FOR UPDATE")
        .bind(document_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(row) = row else {
        return Err(AppError::client(
            StatusCode::NOT_FOUND,
            "document not found",
        ));
    };
    let status: String = row.get("status");
    let object_key: String = row.get("object_key");

    // Already tombstoned: another delete owns it, or a crashed saga left it for the sweep. Don't
    // re-run — it is on its way out. Idempotent accept.
    if status == "deleting" {
        return Ok(StatusCode::ACCEPTED);
    }
    let was_processing = status == "processing";

    // The tombstone. Committed alone, so the row leaves listings and the worker is fenced before any
    // store is touched. `processing_started_at` is deliberately left intact: the deferred sweep uses
    // it as the "worker has released" clock.
    sqlx::query("UPDATE documents SET status = 'deleting' WHERE id = $1")
        .bind(document_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    if was_processing {
        return Ok(StatusCode::ACCEPTED);
    }

    // The stores, then the record. Vectors → object → row, the order that fails toward the least-bad
    // orphan (surviving vectors would still answer questions; a surviving row is an inert ghost).
    delete_document_stores(&state, &actor.tenant_id, document_id, &object_key).await?;

    // Answers that quoted this document are redacted, not left standing. `messages` never held the
    // passages, but an answer derived from them routinely recites them — so erasing the source and
    // leaving the recitation is an erasure with a hole in it.
    let redacted =
        crate::erasure::redact_messages_citing(&state.db, &actor.tenant_id, document_id).await?;
    if redacted > 0 {
        tracing::info!(document = %document_id, "redacted {redacted} conversation turn(s)");
    }

    let mut tx = crate::db::tenant_tx(&state.db, &actor.tenant_id).await?;
    // Guarded on `deleting` so we only remove the tombstone we set, never a row a concurrent path
    // has since re-created or moved on.
    sqlx::query("DELETE FROM documents WHERE id = $1 AND status = 'deleting'")
        .bind(document_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    crate::erasure::finish(&state.db, audit_id, None, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Delete a document's vectors and object. Idempotent — deleting absent vectors or an absent object
/// is a no-op — so it is safe to re-run, which the reaper sweep will. Shared so the two paths cannot
/// diverge on the order or the filters.
async fn delete_document_stores(
    state: &AppState,
    tenant_id: &str,
    document_id: uuid::Uuid,
    object_key: &str,
) -> Result<(), AppError> {
    // Filter on BOTH document_id and tenant_id. `document_id` is a globally-unique UUID, so tenant_id
    // is not strictly required — but isolation is layered precisely so no single condition is load-
    // bearing (invariant 1's philosophy): a bug that ever collided document_ids still could not cross
    // a tenant boundary.
    state
        .qdrant
        .delete_points(
            DeletePointsBuilder::new(COLLECTION)
                .points(Filter::must([
                    Condition::matches("document_id", document_id.to_string()),
                    Condition::matches("tenant_id", tenant_id.to_string()),
                ]))
                .wait(true),
        )
        .await?;

    // Delete the STORED key, never one reconstructed from (tenant, id): the deprecated multipart path
    // wrote `{tenant}/{id}` while the live path writes `tenants/{tenant}/documents/{id}/original.{ext}`,
    // so a reconstruction would miss a legacy object and silently orphan its bytes. Idempotent: MinIO
    // succeeds on an absent key, and an `uploading` row may have no object at all.
    state.s3.delete_object(object_key).await?;

    Ok(())
}

/// Validate and canonicalise an allow-list for a key of `kind`.
///
/// Lives here, beside `insert_api_key`, for the same reason that function is shared: the admin and
/// self-serve mint paths must not drift, and neither may produce a key that cannot work. A
/// publishable key is matched against the `Origin` header by string equality (`auth.rs`), so an
/// un-canonical entry is dead rather than lax, and an *empty* list is not a permissive key — it is a
/// key that 403s on every request, forever.
pub(crate) fn checked_origins(kind: &str, raw: &[String]) -> Result<Vec<String>, AppError> {
    let mut out = Vec::with_capacity(raw.len());
    for origin in raw {
        let normalized = auth::normalize_origin(origin).ok_or_else(|| {
            AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "'{origin}' is not a valid origin; expected scheme://host[:port], e.g. https://example.com"
                ),
            )
        })?;
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }

    // Only publishable keys are origin-checked, so only they are broken by an empty list. Secret keys
    // ignore the field entirely; requiring one there would break the admin path for no gain.
    if kind == "publishable" && out.is_empty() {
        return Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            "a publishable key needs at least one allowed origin, or it cannot answer from anywhere",
        ));
    }

    Ok(out)
}

/// Insert a fresh API key for a tenant and return the raw key (shown ONCE, only its hash stored).
/// Shared by the admin `mint_key` and the self-serve `/auth/keys` handler so the two mint paths
/// cannot drift. Generic over the executor so it works on the pool or inside a transaction.
pub(crate) async fn insert_api_key<'e, E>(
    exec: E,
    tenant_id: &str,
    kind: &str,
    label: &str,
    allowed_origins: &[String],
) -> Result<String, AppError>
where
    E: PgExecutor<'e>,
{
    // Before the insert, not after: a stored dead key is indistinguishable from a live one until a
    // real visitor hits it.
    let allowed_origins = checked_origins(kind, allowed_origins)?;

    let raw = auth::generate_key(kind);
    sqlx::query(
        "INSERT INTO api_keys (key_hash, tenant_id, kind, label, allowed_origins) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(auth::hash_key(&raw))
    .bind(tenant_id)
    .bind(kind)
    .bind(label)
    .bind(&allowed_origins) // &[String] -> text[]
    .execute(exec)
    .await?;
    Ok(raw)
}

/// Create a tenant row + its initial `sk_` secret key, returning the raw key (shown once).
/// Shared by `POST /admin/tenants` and `POST /auth/register` so the two provisioning paths can't
/// drift. Takes a `&mut PgConnection` so a caller that needs atomicity (register: tenant + account
/// + session in one unit) can hand it a transaction.
pub(crate) async fn provision_tenant(
    conn: &mut PgConnection,
    slug: &str,
    name: &str,
) -> Result<String, AppError> {
    // The slug is interpolated into every object key, and the key is what a presigned URL
    // authorises. A slug like `a/../b` would escape its own prefix. The DB has the same CHECK;
    // this exists to return a 400 rather than a 500.
    if !upload::key::is_valid_slug(slug) {
        return Err(AppError::client(
            StatusCode::BAD_REQUEST,
            "tenant id must match ^[a-z0-9][a-z0-9-]{0,62}$",
        ));
    }

    let inserted =
        sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
            .bind(slug)
            .bind(name)
            .execute(&mut *conn)
            .await?;
    if inserted.rows_affected() == 0 {
        return Err(AppError::client(
            StatusCode::CONFLICT,
            "tenant already exists",
        ));
    }

    insert_api_key(&mut *conn, slug, "secret", "default", &[]).await
}

#[derive(Deserialize)]
pub struct CreateTenantRequest {
    id: String, // human-friendly slug, e.g. "umbrella"
    name: String,
}

pub async fn create_tenant(
    _admin: AdminAuth,
    State(state): State<AppState>,
    Json(req): Json<CreateTenantRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let mut conn = state.db.acquire().await?;
    let raw = provision_tenant(&mut conn, &req.id, &req.name).await?;

    Ok((
        StatusCode::CREATED,
        Json(
            json!({ "tenant_id": req.id, "api_key": raw, "note": "store this now; it won't be shown again" }),
        ),
    ))
}

#[derive(Deserialize)]
pub struct MintKeyRequest {
    #[serde(default = "default_kind")]
    kind: String, // "secret" | "publishable"
    #[serde(default)]
    label: String,
    #[serde(default)]
    allowed_origins: Vec<String>, // only meaningful for publishable keys
}
fn default_kind() -> String {
    "secret".to_string()
}

/// `GET /admin/ops/tenants` — which tenants are doing what, **right now**.
///
/// **This exists so `/metrics` never has to carry a tenant label** (invariant 30). The two questions
/// look similar and are not: *"is something wrong?"* needs history and no identity, and belongs in a
/// time series; *"which tenant?"* needs identity and no history, and belongs here.
///
/// Its defining property is what it does **not** do: it stores nothing. The answer is derived
/// entirely from live tables, so it is always current, needs no retention policy, and a tenant
/// erased by phase 12 disappears from it in the same statement — with no extra code and no new
/// obligation. A labelled Prometheus series would have kept them for its whole retention window,
/// outside every erasure guarantee this system makes.
///
/// Loops tenant by tenant through `tenant_tx`, exactly as `reaper.rs` does. Here that is *correct*
/// rather than merely expensive: the result is per-tenant, and RLS is what keeps the loop honest.
/// Do not reach for `metrics_document_counts()` — that function exists precisely because its result
/// has no identity, and this one needs identity.
///
/// Human-triggered and capped. Nothing should poll it.
pub async fn ops_tenants(
    _admin: AdminAuth,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    // `tenants` has no RLS — it is the registry itself.
    let tenants = sqlx::query("SELECT id, name FROM tenants ORDER BY id LIMIT 200")
        .fetch_all(&state.db)
        .await?;

    let mut out = Vec::new();
    for t in &tenants {
        let tenant_id: String = t.get("id");
        let mut tx = crate::db::tenant_tx(&state.db, &tenant_id).await?;

        let docs = sqlx::query(
            "SELECT status, count(*) AS n FROM documents GROUP BY status ORDER BY status",
        )
        .fetch_all(&mut *tx)
        .await?;
        let recent: i64 = sqlx::query(
            "SELECT count(*) AS n FROM messages WHERE created_at > now() - interval \'1 hour\'",
        )
        .fetch_one(&mut *tx)
        .await?
        .get("n");
        tx.commit().await?;

        out.push(json!({
            "tenant_id": tenant_id,
            "name": t.get::<String, _>("name"),
            "documents": docs.iter().map(|d| json!({
                "status": d.get::<String, _>("status"),
                "count": d.get::<i64, _>("n"),
            })).collect::<Vec<_>>(),
            "messages_last_hour": recent,
        }));
    }

    // Busiest first: in an incident the question is "who is causing this", and the answer is usually
    // at the top.
    out.sort_by_key(|t| -t["messages_last_hour"].as_i64().unwrap_or(0));
    Ok(Json(json!({ "tenants": out })))
}

/// `DELETE /admin/tenants/{tenant_id}` — erase a tenant and everything it owns.
///
/// **Admin-only, and that is the point.** Every other erasure in this system is something a tenant
/// does to their own data. This one ends the tenant, so it is guarded by `ADMIN_API_KEY` — a
/// deployment secret, not a database row — for the same reason tenant *creation* is: these are the
/// operations that make and unmake the tenancy registry itself.
///
/// Returns `200` with what was removed, rather than `204`. A caller acting on an erasure request
/// needs evidence, and "how many vectors and objects went" is the evidence. The same numbers land in
/// `erasures`, which survives the deletion because it holds no foreign key to `tenants`.
///
/// An unknown tenant is a `404` — and here that is not a non-oracle concern (the admin key already
/// sees every tenant) but simple honesty: reporting success for a tenant that never existed would
/// let a typo read as a completed erasure.
pub async fn delete_tenant(
    _admin: AdminAuth,
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    // `tenants` has no RLS — it is the registry itself — so this is the plain pool.
    let exists = sqlx::query("SELECT 1 FROM tenants WHERE id = $1")
        .bind(&tenant_id)
        .fetch_optional(&state.db)
        .await?
        .is_some();
    if !exists {
        return Err(AppError::client(StatusCode::NOT_FOUND, "tenant not found"));
    }

    let audit_id = crate::erasure::begin(&state.db, &tenant_id, "tenant", None, "admin").await?;
    let (vectors, objects) = crate::erasure::erase_tenant(&state, &tenant_id).await?;
    crate::erasure::finish(&state.db, audit_id, Some(vectors), Some(objects)).await?;

    tracing::warn!(
        tenant = %tenant_id,
        "tenant erased: {vectors} vector(s), {objects} object(s), all rows"
    );
    Ok(Json(json!({
        "tenant_id": tenant_id,
        "vectors_deleted": vectors,
        "objects_deleted": objects,
        "erasure_id": audit_id.to_string(),
    })))
}

pub async fn mint_key(
    _admin: AdminAuth,
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
    Json(req): Json<MintKeyRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    if req.kind != "secret" && req.kind != "publishable" {
        return Err(AppError::client(
            StatusCode::BAD_REQUEST,
            "kind must be 'secret' or 'publishable'",
        ));
    }

    // Without this the INSERT below trips api_keys_tenant_id_fkey and the caller gets an opaque
    // 500 that says nothing about the actual mistake — a tenant that was never created.
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

    let label = if req.label.is_empty() {
        "default"
    } else {
        &req.label
    };

    let raw = insert_api_key(
        &state.db,
        &tenant_id,
        &req.kind,
        label,
        &req.allowed_origins,
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(
            json!({ "tenant_id": tenant_id, "kind": req.kind, "allowed_origins": req.allowed_origins, "api_key": raw }),
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> Result<AskRequest, serde_json::Error> {
        serde_json::from_str(body)
    }

    /// The reported bug: a client holding empty conversation state sent `""` and got a 400 on the
    /// very first turn — the request that is supposed to CREATE the conversation.
    #[test]
    fn empty_conversation_id_starts_a_new_conversation() {
        let req = parse(r#"{"query":"who is ridwan?","limit":3,"conversation_id":""}"#).unwrap();
        assert_eq!(req.conversation_id, None);
    }

    #[test]
    fn whitespace_and_null_are_also_absent() {
        assert_eq!(
            parse(r#"{"query":"q","conversation_id":"   "}"#)
                .unwrap()
                .conversation_id,
            None
        );
        assert_eq!(
            parse(r#"{"query":"q","conversation_id":null}"#)
                .unwrap()
                .conversation_id,
            None
        );
    }

    /// Regression guard: `deserialize_with` does not imply `default`, so an absent key must still
    /// work. Dropping `#[serde(default)]` would break every existing caller.
    #[test]
    fn omitted_conversation_id_still_works() {
        let req = parse(r#"{"query":"q"}"#).unwrap();
        assert_eq!(req.conversation_id, None);
        assert_eq!(req.limit, default_limit());
    }

    #[test]
    fn a_real_uuid_is_carried_through() {
        let req =
            parse(r#"{"query":"q","conversation_id":"7045945d-3a0e-4b69-9749-326871ef7516"}"#)
                .unwrap();
        assert_eq!(
            req.conversation_id,
            Some(uuid::Uuid::parse_str("7045945d-3a0e-4b69-9749-326871ef7516").unwrap())
        );
    }

    /// Leniency stops at empty. A garbage id is a client bug and must not be silently swallowed
    /// into "new conversation", which would lose the user's history without telling anyone.
    #[test]
    fn a_malformed_conversation_id_is_still_an_error() {
        assert!(parse(r#"{"query":"q","conversation_id":"not-a-uuid"}"#).is_err());
    }

    /// The listing's page size. The default is the load-bearing part: it is what an un-updated
    /// client receives, and therefore what actually closes the unbounded read.
    mod page_limit {
        use super::*;

        #[test]
        fn absent_means_the_default_not_unbounded() {
            assert_eq!(checked_page_limit(None).unwrap(), DEFAULT_PAGE_LIMIT);
        }

        #[test]
        fn a_caller_cannot_reinstate_the_unbounded_read() {
            // The entire point of the cap. `?limit=100000` was the obvious way to undo this change.
            assert!(checked_page_limit(Some(MAX_PAGE_LIMIT + 1)).is_err());
            assert!(checked_page_limit(Some(u64::MAX)).is_err());
            assert!(checked_page_limit(Some(MAX_PAGE_LIMIT)).is_ok());
        }

        #[test]
        fn zero_is_a_caller_error_not_an_empty_page() {
            assert!(checked_page_limit(Some(0)).is_err());
        }
    }

    /// The keyset cursor. Its failure mode is silent — a cursor that round-trips wrongly skips or
    /// repeats rows at a page boundary and looks like a working listing.
    mod cursor {
        use super::*;

        const TS: &str = "2026-07-20 04:09:35.353682+00";
        const ID: &str = "5d2810fc-4117-4b34-b4a4-37009bffee40";

        #[test]
        fn a_minted_cursor_round_trips_exactly() {
            let id = uuid::Uuid::parse_str(ID).unwrap();
            let parsed = parse_cursor(&encode_cursor(TS, id)).unwrap();
            assert_eq!(parsed.id, id);
            // The microseconds must survive: they are what separates two rows written in the same
            // second, and losing them silently drops rows at the boundary.
            assert_eq!(parsed.created_at, "2026-07-20T04:09:35.353682+00");
        }

        #[test]
        fn the_space_separator_becomes_a_t() {
            // Postgres renders `timestamptz::text` with a space. A raw space in a query parameter
            // is the `%20`-or-`+` ambiguity, and `+` decodes to a space — which would corrupt the
            // offset, not just the separator.
            let encoded = encode_cursor(TS, uuid::Uuid::parse_str(ID).unwrap());
            assert!(!encoded.contains(' '), "cursor must be URL-safe: {encoded}");
            assert!(encoded.starts_with("2026-07-20T04:09:35"));
        }

        #[test]
        fn only_the_first_space_is_replaced() {
            // `replacen(.., 1)`: the separator is the only space in a timestamp, and a blanket
            // `replace` would silently rewrite anything that followed.
            let e = encode_cursor("2026-07-20 04:09:35+00", uuid::Uuid::nil());
            assert_eq!(e.matches('T').count(), 1);
        }

        #[test]
        fn garbage_is_a_client_error_and_never_reaches_postgres() {
            // Each of these would otherwise be interpolated into a `::timestamptz` cast, where the
            // database raises an error that `?` turns into a 500 — an internal error for what is
            // plainly the caller's malformed input.
            for bad in [
                "",
                "nonsense",
                "~",                                  // no halves
                "2026-07-20T04:09:35+00",             // no id
                "2026-07-20T04:09:35+00~not-a-uuid",  // bad id
                "not-a-date~5d2810fc-4117-4b34-b4a4-37009bffee40",
                // Shape-valid, range-invalid: passes any "looks like a date" check and still fails
                // the cast. This is the case a character-class validator would let through.
                "9999-99-99T99:99:99+00~5d2810fc-4117-4b34-b4a4-37009bffee40",
                "2026-13-01T00:00:00+00~5d2810fc-4117-4b34-b4a4-37009bffee40",
                "2026-07-20T24:00:00+00~5d2810fc-4117-4b34-b4a4-37009bffee40",
                // An offset we never emit — accepting it would mean comparing against a moment
                // other than the one the cursor names.
                "2026-07-20T04:09:35+05:30~5d2810fc-4117-4b34-b4a4-37009bffee40",
                // Injection through the timestamp half.
                "2026-07-20T04:09:35+00'; DROP TABLE documents;--~5d2810fc-4117-4b34-b4a4-37009bffee40",
            ] {
                assert!(parse_cursor(bad).is_err(), "should have rejected: {bad:?}");
            }
        }

        #[test]
        fn the_forms_postgres_actually_emits_are_accepted() {
            for good in [
                "2026-07-20T04:09:35+00",        // whole second
                "2026-07-20T04:09:35.3+00",      // trailing zeros trimmed by Postgres
                "2026-07-20T04:09:35.353682+00", // full microseconds
                "2026-07-20T04:09:35",           // no offset
                "2026-07-20T04:09:35Z",
                "2026-07-20T04:09:35+00:00",
                "2026-12-31T23:59:60+00", // leap second; Postgres accepts it
            ] {
                assert!(
                    is_our_timestamp(good),
                    "should have accepted: {good:?} — Postgres emits this and a cursor built from \
                     it would be rejected as invalid"
                );
            }
        }

        #[test]
        fn fractional_seconds_are_bounded() {
            assert!(!is_our_timestamp("2026-07-20T04:09:35.+00")); // a dot with no digits
            assert!(!is_our_timestamp("2026-07-20T04:09:35.1234567+00")); // 7 digits: not ours
        }
    }
}
