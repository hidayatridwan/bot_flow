mod chunk;
mod event;
mod lifecycle;
mod parser;
mod reaper;
#[cfg(test)]
mod testsupport;

use anyhow::Context;
use common::embedding::{EmbedError, EmbeddingClient};
use futures_lite::StreamExt;
use lapin::{
    options::{
        BasicAckOptions, BasicConsumeOptions, BasicNackOptions, BasicQosOptions,
        ExchangeDeclareOptions, QueueBindOptions, QueueDeclareOptions,
    },
    types::{AMQPValue, FieldTable, ShortString},
    Channel, Connection, ConnectionProperties, ExchangeKind,
};
use qdrant_client::qdrant::{
    Condition, DeletePointsBuilder, Filter, PointStruct, UpsertPointsBuilder,
};
use qdrant_client::Qdrant;
use s3::{creds::Credentials, Bucket, Region};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;

/// Exchange MinIO publishes bucket notifications to. It does NOT declare the queue, so the
/// binding below must exist before notifications are enabled or events vanish into the exchange.
const EVENTS_EXCHANGE: &str = "minio.events";
const EVENTS_ROUTING_KEY: &str = "document.uploaded";
const EVENTS_QUEUE: &str = "document_events";
const DLX_EXCHANGE: &str = "doc.dlx";
const DLQ_QUEUE: &str = "document_events.dlq";

/// Legacy queue for the deprecated proxy upload path. Drained, then deleted.
const LEGACY_QUEUE: &str = "ingest_jobs";

/// Must match the API's collection (handlers.rs). The API owns its creation at startup.
const COLLECTION: &str = "documents";

struct Ctx {
    bucket: Box<Bucket>,
    // Arc so the reaper's delete-sweep can share the one client (phase 8). Qdrant methods take
    // `&self`, so `Arc<Qdrant>` derefs transparently at every existing call site.
    qdrant: Arc<Qdrant>,
    embedder: EmbeddingClient,
    db: PgPool,
    max_upload_bytes: i64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let addr = std::env::var("RABBITMQ_URL").context("RABBITMQ_URL not set")?;

    let bucket = build_bucket()?;
    tracing::info!("S3 bucket ready");

    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&std::env::var("APP_DATABASE_URL").context("APP_DATABASE_URL not set")?)
        .await
        .context("failed to connect to Postgres")?;
    tracing::info!("connected to Postgres");

    let qdrant = Arc::new(
        Qdrant::from_url(&std::env::var("QDRANT_URL").context("QDRANT_URL not set")?)
            .build()
            .context("failed to build Qdrant client")?,
    );
    tracing::info!("Qdrant client ready");

    // Must agree with the API's client, or the two write vectors the other cannot search. Both read
    // the same three vars, and both fall back to LLM_BASE_URL for the endpoint but never for the key.
    let embedder = EmbeddingClient::new(
        std::env::var("EMBEDDING_BASE_URL")
            .or_else(|_| std::env::var("LLM_BASE_URL"))
            .context("neither EMBEDDING_BASE_URL nor LLM_BASE_URL is set")?,
        std::env::var("EMBEDDING_API_KEY").context("EMBEDDING_API_KEY not set")?,
        std::env::var("EMBEDDING_MODEL").unwrap_or_else(|_| "text-embedding-3-small".to_string()),
    );

    let ctx = Arc::new(Ctx {
        bucket: bucket.clone(),
        qdrant: Arc::clone(&qdrant),
        embedder,
        db: db.clone(),
        max_upload_bytes: std::env::var("MAX_UPLOAD_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(25 * 1024 * 1024),
    });

    // The reaper shares the Qdrant client and a clone of the bucket so its delete-sweep can finish
    // deferred deletions across all three stores (phase 8, invariant 10).
    reaper::spawn(db, qdrant, bucket, Duration::from_secs(60));

    // Reconnect forever. A broker restart must not kill the worker: MinIO buffers events to disk
    // while RabbitMQ is down (QUEUE_DIR) and replays them, so all we have to do is be there when
    // it comes back. Exiting here would strand every buffered event.
    loop {
        match broker_session(&ctx, &addr).await {
            Ok(()) => tracing::warn!("broker session ended; reconnecting"),
            Err(e) => tracing::error!("broker session failed: {e:#}; reconnecting"),
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// One connection's lifetime: declare, consume, and return when the connection drops.
async fn broker_session(ctx: &Arc<Ctx>, addr: &str) -> anyhow::Result<()> {
    // Force lapin onto our tokio runtime (instead of its default async-global-executor).
    let conn = Connection::connect(
        addr,
        ConnectionProperties::default()
            .with_executor(tokio_executor_trait::Tokio::current())
            .with_reactor(tokio_reactor_trait::Tokio),
    )
    .await
    .context("failed to connect to RabbitMQ")?;

    let channel = conn.create_channel().await?;
    declare_topology(&channel).await?;

    // One unacked message at a time. Without this the first worker to connect grabs the whole
    // backlog and a second worker sits idle.
    channel
        .basic_qos(1, BasicQosOptions::default())
        .await
        .context("failed to set prefetch")?;

    // Drain the deprecated proxy path alongside the new one, so `POST /documents` keeps working
    // until its clients migrate. A second channel keeps their prefetch independent; the task ends
    // on its own when the connection drops.
    let legacy_channel = conn.create_channel().await?;
    consume_legacy(ctx.clone(), legacy_channel).await?;

    tracing::info!("worker ready, waiting for events on '{EVENTS_QUEUE}'");
    consume_events(ctx.clone(), &channel).await
}

#[derive(serde::Deserialize)]
struct IngestJob {
    document_id: String,
    tenant_id: String,
    object_key: String,
    filename: String,
}

/// DEPRECATED. Consumes jobs published by the API's proxy upload handler. Deleted together with
/// `POST /documents` and `crates/api/src/queue.rs`.
async fn consume_legacy(ctx: Arc<Ctx>, channel: Channel) -> anyhow::Result<()> {
    channel
        .queue_declare(
            LEGACY_QUEUE,
            QueueDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await
        .context("failed to declare legacy queue")?;
    channel.basic_qos(1, BasicQosOptions::default()).await?;

    let mut consumer = channel
        .basic_consume(
            LEGACY_QUEUE,
            "worker-legacy",
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await
        .context("failed to start legacy consumer")?;

    tokio::spawn(async move {
        tracing::info!("legacy consumer draining '{LEGACY_QUEUE}'");
        while let Some(delivery) = consumer.next().await {
            let Ok(delivery) = delivery else { break };
            let outcome = async {
                let job: IngestJob =
                    serde_json::from_slice(&delivery.data).context("invalid job payload")?;
                let doc_uuid = uuid::Uuid::parse_str(&job.document_id)?;
                lifecycle::claim(&ctx.db, &job.tenant_id, doc_uuid, 0, "legacy").await?;
                let n = ingest(
                    &ctx,
                    &job.tenant_id,
                    &doc_uuid,
                    &job.object_key,
                    &job.filename,
                )
                .await?;
                lifecycle::mark_ready(&ctx.db, &job.tenant_id, doc_uuid).await?;
                anyhow::Ok(n)
            }
            .await;

            match outcome {
                Ok(n) => {
                    tracing::info!("legacy: indexed {n} chunks");
                    let _ = delivery.ack(BasicAckOptions::default()).await;
                }
                Err(e) => {
                    tracing::error!("legacy job failed: {e:#}");
                    // Classic queue with no delivery limit: requeueing a poison message would
                    // loop forever, so drop it. The row is left `failed` for the operator.
                    let _ = delivery
                        .nack(BasicNackOptions {
                            requeue: false,
                            ..Default::default()
                        })
                        .await;
                }
            }
        }
    });
    Ok(())
}

/// Declare everything MinIO and the DLQ need. Idempotent, and it must run before MinIO
/// notifications are switched on: a publish to an exchange with no binding is silently dropped.
async fn declare_topology(channel: &Channel) -> anyhow::Result<()> {
    let durable = ExchangeDeclareOptions {
        durable: true,
        ..Default::default()
    };
    channel
        .exchange_declare(
            EVENTS_EXCHANGE,
            ExchangeKind::Direct,
            durable,
            FieldTable::default(),
        )
        .await
        .context("failed to declare events exchange")?;
    channel
        .exchange_declare(
            DLX_EXCHANGE,
            ExchangeKind::Fanout,
            durable,
            FieldTable::default(),
        )
        .await
        .context("failed to declare dead-letter exchange")?;

    // A quorum queue gives us `x-delivery-limit`, so RabbitMQ counts redeliveries and dead-letters
    // on its own. That replaces a hand-rolled TTL/DLX retry loop and a retry counter in the payload.
    let mut args = FieldTable::default();
    args.insert(
        ShortString::from("x-queue-type"),
        AMQPValue::LongString("quorum".into()),
    );
    args.insert(ShortString::from("x-delivery-limit"), AMQPValue::LongInt(5));
    args.insert(
        ShortString::from("x-dead-letter-exchange"),
        AMQPValue::LongString(DLX_EXCHANGE.into()),
    );
    channel
        .queue_declare(
            EVENTS_QUEUE,
            QueueDeclareOptions {
                durable: true,
                ..Default::default()
            },
            args,
        )
        .await
        .context("failed to declare events queue")?;

    channel
        .queue_bind(
            EVENTS_QUEUE,
            EVENTS_EXCHANGE,
            EVENTS_ROUTING_KEY,
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await
        .context("failed to bind events queue")?;

    channel
        .queue_declare(
            DLQ_QUEUE,
            QueueDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;
    channel
        .queue_bind(
            DLQ_QUEUE,
            DLX_EXCHANGE,
            "",
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;

    tracing::info!("topology declared: {EVENTS_EXCHANGE} -> {EVENTS_QUEUE} (dlx: {DLQ_QUEUE})");
    Ok(())
}

async fn consume_events(ctx: Arc<Ctx>, channel: &Channel) -> anyhow::Result<()> {
    let mut consumer = channel
        .basic_consume(
            EVENTS_QUEUE,
            "worker",
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await
        .context("failed to start consumer")?;

    while let Some(delivery) = consumer.next().await {
        // A delivery error means the connection is gone. Return so the caller reconnects rather
        // than propagating out of main and killing the process.
        let delivery = match delivery {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("consumer stream ended: {e}");
                return Ok(());
            }
        };
        match handle(&ctx, &delivery.data).await {
            Ok(()) => delivery.ack(BasicAckOptions::default()).await?,
            Err(Fatal(e)) => {
                // Unparseable / unroutable. Retrying can never make it succeed, so ack it away
                // rather than let it loop until the delivery limit burns down.
                tracing::error!("discarding poison event: {e:#}");
                delivery.ack(BasicAckOptions::default()).await?;
            }
            Err(Retryable(e)) => {
                tracing::error!("event failed, will retry: {e:#}");
                // requeue: true — the quorum queue's x-delivery-limit dead-letters after 5 tries.
                delivery
                    .nack(BasicNackOptions {
                        requeue: true,
                        ..Default::default()
                    })
                    .await?;
            }
        }
    }
    Ok(())
}

/// Distinguishes "this will never work" from "try again later". Acking the first kind is what
/// keeps a poison message from cycling; nacking the second is what makes a transient failure
/// (Qdrant restart, LLM blip) recover on its own.
enum Failure {
    Fatal(anyhow::Error),
    Retryable(anyhow::Error),
}
use Failure::{Fatal, Retryable};

async fn handle(ctx: &Ctx, body: &[u8]) -> Result<(), Failure> {
    let Some(obj) = event::parse(body).map_err(Fatal)? else {
        return Ok(()); // not an ObjectCreated event
    };

    tracing::info!(
        tenant = %obj.tenant_id, document = %obj.document_id, size = obj.size,
        "received upload event"
    );

    // A presigned PUT cannot enforce a body size — this is the only place the cap can be applied.
    // The bandwidth is already spent by now; all we can do is refuse to keep the object.
    if obj.size > ctx.max_upload_bytes {
        let reason = format!(
            "object is {} bytes, limit is {}",
            obj.size, ctx.max_upload_bytes
        );
        tracing::warn!("quarantining {}: {reason}", obj.document_id);
        lifecycle::mark_quarantined(&ctx.db, &obj.tenant_id, obj.document_id, &reason)
            .await
            .map_err(Retryable)?;
        let _ = ctx.bucket.delete_object(&obj.object_key).await;
        return Ok(());
    }

    match lifecycle::claim(
        &ctx.db,
        &obj.tenant_id,
        obj.document_id,
        obj.size,
        &obj.etag,
    )
    .await
    .map_err(Retryable)?
    {
        lifecycle::Claim::Skip(why) => {
            tracing::info!("skipping {}: {why}", obj.document_id);
            return Ok(());
        }
        lifecycle::Claim::Proceed => {}
    }

    // The key ends in `original.{ext}`, which is what the parser dispatches on.
    let result = verify_and_ingest(ctx, &obj).await;
    finish(ctx, &obj.tenant_id, obj.document_id, result).await
}

/// Shared tail of both ingest paths: record the outcome, and classify the failure for the queue.
async fn finish(
    ctx: &Ctx,
    tenant_id: &str,
    document_id: uuid::Uuid,
    result: anyhow::Result<usize>,
) -> Result<(), Failure> {
    match result {
        Ok(n) => {
            if lifecycle::mark_ready(&ctx.db, tenant_id, document_id)
                .await
                .map_err(Retryable)?
            {
                tracing::info!("indexed {n} chunks for document {document_id}");
            } else {
                // The row left `processing` while we were indexing — a delete tombstoned it, or the
                // reaper reclaimed a stale lease. Not ours to finish; the {n} chunks just written are
                // orphans the delete sweep clears by document_id. Do NOT resurrect it (invariant 10).
                tracing::warn!(
                    "document {document_id} left `processing` during indexing (deleted or reclaimed); \
                     {n} chunk(s) written will be swept"
                );
            }
            Ok(())
        }
        Err(e) => {
            let msg = format!("{e:#}");
            // Record the failure, but keep the error for the nack + delivery-limit machinery.
            let _ = lifecycle::mark_failed(&ctx.db, tenant_id, document_id, &msg).await;
            // Fatal acks, which destroys the document; Retryable dead-letters it after the delivery
            // limit, which preserves it. So only a document that can never embed is Fatal — a bad
            // EMBEDDING_API_KEY is the operator's problem, not the document's. See EmbedError::is_fatal.
            if e.downcast_ref::<EmbedError>()
                .is_some_and(EmbedError::is_fatal)
            {
                Err(Fatal(e))
            } else {
                Err(Retryable(e))
            }
        }
    }
}

/// Confirm the object matches what the event claimed, then index it.
async fn verify_and_ingest(ctx: &Ctx, obj: &event::UploadedObject) -> anyhow::Result<usize> {
    // Guards against an event that outlived its object (deleted between publish and delivery).
    let (head, code) = ctx.bucket.head_object(&obj.object_key).await?;
    if code != 200 {
        anyhow::bail!("object '{}' not found (status {code})", obj.object_key);
    }
    if let Some(len) = head.content_length {
        if len != obj.size {
            anyhow::bail!("object size {len} does not match event size {}", obj.size);
        }
    }

    ingest(
        ctx,
        &obj.tenant_id,
        &obj.document_id,
        &obj.object_key,
        &obj.object_key, // key ends in `original.{ext}`; the parser only needs the extension
    )
    .await
}

fn build_bucket() -> anyhow::Result<Box<Bucket>> {
    let endpoint = std::env::var("S3_ENDPOINT").context("S3_ENDPOINT not set")?;
    let name = std::env::var("S3_BUCKET").unwrap_or_else(|_| "documents".to_string());
    let access = std::env::var("S3_ACCESS_KEY").context("S3_ACCESS_KEY not set")?;
    let secret = std::env::var("S3_SECRET_KEY").context("S3_SECRET_KEY not set")?;
    let region = std::env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());

    let region = Region::Custom { region, endpoint };
    let creds = Credentials::new(Some(&access), Some(&secret), None, None, None)?;
    Ok(Bucket::new(&name, region, creds)?.with_path_style())
}

/// Download → parse → chunk → embed → index. Shared by the event path and the deprecated
/// proxy path, which differ only in how they learn about the object.
///
/// `filename_hint` exists solely so the parser can read an extension off it; the legacy path's
/// object keys carry none.
async fn ingest(
    ctx: &Ctx,
    tenant_id: &str,
    doc_uuid: &uuid::Uuid,
    object_key: &str,
    filename_hint: &str,
) -> anyhow::Result<usize> {
    let resp = ctx.bucket.get_object(object_key).await?;
    if resp.status_code() != 200 {
        anyhow::bail!(
            "download failed for '{object_key}' (status {})",
            resp.status_code()
        );
    }
    let bytes = resp.bytes();

    let document_id = doc_uuid.to_string();
    let text = parser::parse_to_text(bytes, filename_hint, &document_id).await?;
    let chunks = chunk::chunk_text(&text, 800, 100);
    if chunks.is_empty() {
        tracing::warn!("no extractable text in '{object_key}'");
        return Ok(0);
    }
    tracing::info!(
        "parsed {} chars -> {} chunks",
        text.chars().count(),
        chunks.len()
    );

    // Batched internally: a large document is more chunks than one request may carry.
    let vectors = ctx.embedder.embed_batch(&chunks).await?;

    // Drop whatever a previous attempt left behind. Deterministic ids alone would overwrite
    // chunks 0..n, but a re-parse yielding FEWER chunks would strand the old tail.
    ctx.qdrant
        .delete_points(
            DeletePointsBuilder::new(COLLECTION)
                .points(Filter::must([Condition::matches(
                    "document_id",
                    document_id.clone(),
                )]))
                .wait(true),
        )
        .await
        .context("failed to clear previous chunks")?;

    // Deterministic point ids: (document_id, chunk_index) always maps to the same id, so a
    // redelivered event overwrites its own chunks instead of duplicating them.
    let points: Vec<PointStruct> = chunks
        .iter()
        .zip(vectors)
        .enumerate()
        .map(|(i, (text, vector))| {
            PointStruct::new(
                point_id(doc_uuid, i).to_string(),
                vector,
                [
                    ("text", text.clone().into()),
                    ("tenant_id", tenant_id.to_string().into()),
                    ("document_id", document_id.clone().into()),
                ],
            )
        })
        .collect();

    let count = points.len();
    ctx.qdrant
        .upsert_points(UpsertPointsBuilder::new(COLLECTION, points).wait(true))
        .await
        .context("failed to upsert points")?;
    Ok(count)
}

/// Stable id for one chunk of one document. UUIDv5 is a hash, not a random draw, so the same
/// inputs always produce the same id — which is what makes re-indexing an overwrite.
fn point_id(document_id: &uuid::Uuid, chunk_index: usize) -> uuid::Uuid {
    uuid::Uuid::new_v5(document_id, chunk_index.to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_ids_are_stable_across_runs() {
        let doc = uuid::Uuid::parse_str("7045945d-3a0e-4b69-9749-326871ef7516").unwrap();
        assert_eq!(point_id(&doc, 0), point_id(&doc, 0));
        assert_ne!(point_id(&doc, 0), point_id(&doc, 1));
        // A different document never collides with this one's chunks.
        let other = uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000000").unwrap();
        assert_ne!(point_id(&doc, 0), point_id(&other, 0));
    }
}
