use crate::auth::{self, AdminAuth, AuthTenant};
use crate::queue::{self, IngestJob};
use crate::rate_limit;
use anyhow::Context;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::Json;
use qdrant_client::qdrant::{
    value::Kind, Condition, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder, Distance,
    FieldType, Filter, KeywordIndexParamsBuilder, PointStruct, QueryPointsBuilder,
    UpsertPointsBuilder, VectorParamsBuilder,
};
use qdrant_client::Qdrant;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::conversation;
use crate::embedding;
use crate::error::AppError;
use crate::state::AppState;
use crate::upload;
use sqlx::Row;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, StreamExt};
use std::convert::Infallible;

/// Qdrant collection name. Hardcoded in Phase 1 — multi-tenancy arrives in Phase 3.
const COLLECTION: &str = "documents";

// The context passages stay numbered: the numbering is what keeps the model anchored to a specific
// passage rather than blending them. It just must not surface those numbers to the reader.
const RAG_SYSTEM_PROMPT: &str = "You are a customer service assistant. Answer the user's question \
    ONLY using the numbered CONTEXT passages below. If the answer is not in the context, say \
    honestly that you don't have that information — do not make anything up. Be concise. \
    Write the answer as plain prose: never include citation markers, bracketed numbers, or any \
    reference to the passage numbers.";

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
    qdrant
        .create_collection(CreateCollectionBuilder::new(COLLECTION).vectors_config(
            VectorParamsBuilder::new(embedding::EMBEDDING_DIM, Distance::Cosine),
        ))
        .await
        .context("failed to create collection")?;

    // Index the tenant_id payload with the multitenancy optimization (is_tenant=true).
    // Created BEFORE ingest (here, when the collection is first born) so HNSW becomes filter-aware.
    qdrant
        .create_field_index(
            CreateFieldIndexCollectionBuilder::new(COLLECTION, "tenant_id", FieldType::Keyword)
                .field_index_params(KeywordIndexParamsBuilder::default().is_tenant(true)),
        )
        .await
        .context("failed to create tenant_id index")?;

    tracing::info!(
        "collection '{COLLECTION}' created (dim={}, cosine) + tenant_id index",
        embedding::EMBEDDING_DIM
    );
    Ok(())
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

#[derive(Deserialize)]
pub struct IngestRequest {
    texts: Vec<String>,
}

pub async fn ingest(
    State(state): State<AppState>,
    tenant: AuthTenant,             // FromRequestParts — reads headers, no body
    Json(req): Json<IngestRequest>, // FromRequest — consumes body, MUST be last
) -> Result<Json<Value>, AppError> {
    tenant.require_secret()?;
    let embedder = state.embedder.clone();
    let texts = req.texts.clone();
    let vectors = tokio::task::spawn_blocking(move || {
        let mut model = embedder.lock().expect("embedder lock poisoned");
        embedding::embed_passages(&mut model, &texts)
    })
    .await??;

    // Global UUID IDs: required since going multi-tenant, otherwise points across tenants
    // overwrite each other. tenant_id goes into the payload → becomes the mandatory filter on reads (Step 3).
    let points: Vec<PointStruct> = req
        .texts
        .iter()
        .zip(vectors)
        .map(|(text, vector)| {
            let id = uuid::Uuid::new_v4().to_string();
            PointStruct::new(
                id,
                vector,
                [
                    ("text", text.clone().into()),
                    ("tenant_id", tenant.tenant_id.clone().into()),
                ],
            )
        })
        .collect();

    let count = points.len();
    state
        .qdrant
        .upsert_points(UpsertPointsBuilder::new(COLLECTION, points).wait(true))
        .await?;

    Ok(Json(json!({ "ingested": count })))
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

pub async fn search(
    State(state): State<AppState>,
    tenant: AuthTenant, // FromRequestParts — reads headers, no body
    Json(req): Json<SearchRequest>,
) -> Result<Json<Value>, AppError> {
    let embedder = state.embedder.clone();
    let query = req.query.clone();
    let vector = tokio::task::spawn_blocking(move || {
        let mut model = embedder.lock().expect("embedder lock poisoned");
        embedding::embed_query(&mut model, &query)
    })
    .await??;

    let response = state
        .qdrant
        .query(
            QueryPointsBuilder::new(COLLECTION)
                .query(vector)
                .limit(req.limit)
                .filter(tenant_filter(&tenant.tenant_id))
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

    Ok(Json(json!({ "hits": hits })))
}

const NO_ANSWER: &str = "Sorry, I couldn't find any relevant information.";

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

pub async fn ask(
    State(state): State<AppState>,
    tenant: AuthTenant,
    Json(req): Json<AskRequest>,
) -> Result<Json<Value>, AppError> {
    rate_limit::check(&state, &tenant.tenant_id).await?;

    let (conversation_id, standalone) = prepare(&state, &tenant.tenant_id, &req).await?;

    let relevant = retrieve(&state, &tenant.tenant_id, &standalone, req.limit).await?;

    if relevant.is_empty() {
        conversation::append_turn(
            &state.db,
            &tenant.tenant_id,
            conversation_id,
            &req.query,
            NO_ANSWER,
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

    let answer = state.llm.answer(RAG_SYSTEM_PROMPT, &user).await?;

    conversation::append_turn(
        &state.db,
        &tenant.tenant_id,
        conversation_id,
        &req.query,
        &answer,
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
    tenant: AuthTenant,
    Json(req): Json<UploadUrlRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    tenant.require_secret()?;
    rate_limit::check(&state, &tenant.tenant_id).await?;

    let session = upload::create_session(
        &state.db,
        &state.s3_public,
        &tenant.tenant_id,
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
    tenant: AuthTenant,
    Path(document_id): Path<uuid::Uuid>,
) -> Result<Json<Value>, AppError> {
    tenant.require_secret()?;
    rate_limit::check(&state, &tenant.tenant_id).await?;

    let session = upload::refresh_session(
        &state.db,
        &state.s3_public,
        &tenant.tenant_id,
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
    let embedder = state.embedder.clone();
    let query_owned = query.to_string();
    let vector = tokio::task::spawn_blocking(move || {
        let mut model = embedder.lock().expect("embedder lock poisoned");
        embedding::embed_query(&mut model, &query_owned)
    })
    .await??;

    let response = state
        .qdrant
        .query(
            QueryPointsBuilder::new(COLLECTION)
                .query(vector)
                .limit(limit)
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

    Ok(hits
        .into_iter()
        .filter(|h| h.score >= state.rag_score_threshold)
        .collect())
}

pub async fn ask_stream(
    State(state): State<AppState>,
    tenant: AuthTenant,
    Json(req): Json<AskRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    rate_limit::check(&state, &tenant.tenant_id).await?;

    // Memory + rewrite before retrieval, so failures here are normal HTTP errors, not mid-stream.
    let (conversation_id, standalone) = prepare(&state, &tenant.tenant_id, &req).await?;

    // Retrieval happens before streaming, so retrieval errors are normal HTTP errors (not mid-stream).
    let relevant = retrieve(&state, &tenant.tenant_id, &standalone, req.limit).await?;

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
    let tenant_id = tenant.tenant_id.clone();
    let question = req.query.clone();

    let sse = async_stream::stream! {
        // 1. the conversation id, so a brand-new conversation can be continued by the client.
        yield Ok::<_, Infallible>(Event::default().event("conversation").data(conversation_id.to_string()));

        // 2. sources next.
        yield Ok(Event::default().event("sources").json_data(&sources).unwrap());

        if empty {
            // Persisted so history stays a faithful record of what the user was told.
            if let Err(e) = conversation::append_turn(&db, &tenant_id, conversation_id, &question, NO_ANSWER).await {
                tracing::error!("failed to persist turn: {e:?}");
            }
            yield Ok(Event::default().event("token").data(NO_ANSWER));
            yield Ok(Event::default().event("done").data(""));
            return;
        }

        // 3. stream the answer tokens, accumulating them so the full reply can be stored.
        let mut answer = String::new();
        let mut failed = false;
        match llm.answer_stream(RAG_SYSTEM_PROMPT, &user).await {
            Ok(token_stream) => {
                futures_util::pin_mut!(token_stream); // make it pollable with .next()
                while let Some(item) = token_stream.next().await {
                    match item {
                        Ok(text) if !text.is_empty() => {
                            answer.push_str(&text);
                            yield Ok(Event::default().event("token").data(text));
                        }
                        Ok(_) => {} // empty delta (role-only / [DONE]) — skip
                        Err(e) => {
                            failed = true;
                            yield Ok(Event::default().event("error").data(format!("{e:#}")));
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                failed = true;
                yield Ok(Event::default().event("error").data(format!("{e:#}")));
            }
        }

        // 4. persist the turn. A stream that errored may have emitted a partial answer —
        // storing it would make the next rewrite reason over a truncated sentence.
        if !failed && !answer.is_empty() {
            if let Err(e) = conversation::append_turn(&db, &tenant_id, conversation_id, &question, &answer).await {
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
    tenant: AuthTenant,
) -> Result<Json<Value>, AppError> {
    tenant.require_secret()?;
    let mut tx = crate::db::tenant_tx(&state.db, &tenant.tenant_id).await?;
    // Note: NO `WHERE tenant_id` — RLS scopes this to the current tenant automatically.
    // That's the whole point: forgetting the filter can't leak other tenants' rows.
    let rows = sqlx::query(
        "SELECT id, filename, status, created_at::text AS created_at FROM documents ORDER BY created_at DESC",
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let docs: Vec<Value> = rows
        .iter()
        .map(|r| {
            let id: uuid::Uuid = r.get("id");
            json!({
                "id": id.to_string(),
                "filename": r.get::<String, _>("filename"),
                "status": r.get::<String, _>("status"),
                "created_at": r.get::<String, _>("created_at"),
            })
        })
        .collect();

    Ok(Json(json!({ "documents": docs })))
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
    // The slug is interpolated into every object key, and the key is what a presigned URL
    // authorises. A slug like `a/../b` would escape its own prefix. The DB has the same CHECK;
    // this exists to return a 400 rather than a 500.
    if !upload::key::is_valid_slug(&req.id) {
        return Err(AppError::client(
            StatusCode::BAD_REQUEST,
            "tenant id must match ^[a-z0-9][a-z0-9-]{0,62}$",
        ));
    }

    let inserted =
        sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
            .bind(&req.id)
            .bind(&req.name)
            .execute(&state.db)
            .await?;
    if inserted.rows_affected() == 0 {
        return Err(AppError::client(
            StatusCode::CONFLICT,
            "tenant already exists",
        ));
    }

    // Mint an initial secret key. Raw key shown ONCE; only its hash is stored.
    let raw = auth::generate_key("secret");
    sqlx::query("INSERT INTO api_keys (key_hash, tenant_id, kind, label) VALUES ($1, $2, 'secret', 'default')")
        .bind(auth::hash_key(&raw))
        .bind(&req.id)
        .execute(&state.db)
        .await?;

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
        "default".to_string()
    } else {
        req.label.clone()
    };

    let raw = auth::generate_key(&req.kind);
    sqlx::query(
        "INSERT INTO api_keys (key_hash, tenant_id, kind, label, allowed_origins) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(auth::hash_key(&raw))
    .bind(&tenant_id)
    .bind(&req.kind)
    .bind(&label)
    .bind(&req.allowed_origins) // Vec<String> -> text[]
    .execute(&state.db)
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
        let req = parse(r#"{"query":"q","conversation_id":"7045945d-3a0e-4b69-9749-326871ef7516"}"#)
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
}
