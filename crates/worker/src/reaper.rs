//! Settles documents that no event will ever arrive for.
//!
//! Three kinds of stuck row:
//!   * `uploading` whose presigned URL lapsed — the client never PUT anything.
//!   * `processing` whose worker died mid-job — the lease is stale and nobody will finish it.
//!   * `deleting` — a document mid-erasure whose store-cleanup must be finished or resumed (phase 8).
//!
//! Note the per-tenant loop. `documents` has FORCE ROW LEVEL SECURITY and the worker connects as
//! the non-superuser `app_user`, so a single cross-tenant UPDATE would match zero rows and appear
//! to succeed. The policy compares against `app.current_tenant`, which we must set per tenant.

use qdrant_client::qdrant::{Condition, DeletePointsBuilder, Filter};
use qdrant_client::Qdrant;
use s3::Bucket;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use crate::COLLECTION;

/// Absorbs clock skew and slow uploads: a signature is checked when the PUT *starts*, so a
/// transfer that began just before expiry is legitimate and may still be in flight.
const UPLOAD_GRACE: &str = "5 minutes";

/// How long a worker may hold a document before we assume it died.
const PROCESSING_LEASE: &str = "30 minutes";

pub fn spawn(db: PgPool, qdrant: Arc<Qdrant>, bucket: Box<Bucket>, every: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        loop {
            ticker.tick().await;
            if let Err(e) = sweep(&db, &qdrant, &bucket).await {
                tracing::error!("reaper sweep failed: {e:#}");
            }
        }
    });
}

async fn sweep(db: &PgPool, qdrant: &Qdrant, bucket: &Bucket) -> anyhow::Result<()> {
    // `tenants` has no RLS, so this read needs no tenant context.
    let tenants = sqlx::query("SELECT id FROM tenants").fetch_all(db).await?;

    for t in &tenants {
        let tenant_id: String = t.get("id");

        let expired = sweep_one(
            db,
            &tenant_id,
            &format!(
                "UPDATE documents SET status = 'expired'
                  WHERE status = 'uploading'
                    AND upload_expires_at < now() - interval '{UPLOAD_GRACE}'"
            ),
        )
        .await?;

        // Back to 'failed', not 'uploaded': the object is there, so a redelivered event (or a
        // manual retry) can claim it again. `failed` is an accepted claim source.
        let reclaimed = sweep_one(
            db,
            &tenant_id,
            &format!(
                "UPDATE documents
                    SET status = 'failed',
                        error = 'processing lease expired; worker presumed dead',
                        processing_started_at = null
                  WHERE status = 'processing'
                    AND processing_started_at < now() - interval '{PROCESSING_LEASE}'"
            ),
        )
        .await?;

        let deleted = finish_deletions(db, qdrant, bucket, &tenant_id).await?;

        if expired > 0 || reclaimed > 0 || deleted > 0 {
            tracing::info!(
                tenant = %tenant_id,
                "reaper: {expired} abandoned upload(s) expired, {reclaimed} stale lease(s) reclaimed, \
                 {deleted} deletion(s) finished"
            );
        }
    }
    Ok(())
}

/// Finish the store-cleanup for `deleting` rows — the deferred and crash-recovery half of the
/// deletion saga (phase 8, invariant 10). The synchronous endpoint does this inline for a document no
/// worker is touching; this handles the two cases it cannot:
///
///   * a delete that raced an active index (returned `202`, worker still writing vectors), and
///   * a synchronous delete whose API process crashed mid-saga, leaving a `deleting` row.
///
/// **Which `deleting` rows are safe to finish** is the whole correctness question, and
/// `processing_started_at` answers it. A tombstoned row that was never `processing` has it `NULL` (no
/// worker is involved) → safe now. A row tombstoned *while* `processing` keeps it, and is safe only
/// once the lease has elapsed: past that the worker is provably done or dead, so it cannot upsert
/// vectors again and race our delete. This is the ~lease-long window that entry in *Known state*
/// describes; it only affects a delete that lands during active indexing, which is rare.
async fn finish_deletions(
    db: &PgPool,
    qdrant: &Qdrant,
    bucket: &Bucket,
    tenant_id: &str,
) -> anyhow::Result<u64> {
    // Read the candidates under tenant RLS, then release the transaction before the store calls —
    // the tombstone is durable and nothing moves a `deleting` row, so there is no lock to hold.
    let mut tx = tenant_tx(db, tenant_id).await?;
    let rows = sqlx::query(&format!(
        "SELECT id, object_key FROM documents
          WHERE status = 'deleting'
            AND (processing_started_at IS NULL
                 OR processing_started_at < now() - interval '{PROCESSING_LEASE}')"
    ))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut done = 0u64;
    for row in &rows {
        let id: Uuid = row.get("id");
        let object_key: String = row.get("object_key");

        // Vectors → object → row, the same order and filters as the API's `delete_document_stores`
        // (handlers.rs). The two are separate crates and this duplication is deliberate — sharing it
        // would drag the Qdrant and S3 clients into `common` for fifteen lines — but they must not
        // drift: the order is what keeps a crash from stranding queryable vectors, and both filter
        // document_id AND tenant_id so no single condition is load-bearing (invariant 1's layering).
        qdrant
            .delete_points(
                DeletePointsBuilder::new(COLLECTION)
                    .points(Filter::must([
                        Condition::matches("document_id", id.to_string()),
                        Condition::matches("tenant_id", tenant_id.to_string()),
                    ]))
                    .wait(true),
            )
            .await?;

        // Idempotent: MinIO succeeds on an absent key, and an `uploading` row may have no object.
        bucket.delete_object(&object_key).await?;

        let mut tx = tenant_tx(db, tenant_id).await?;
        let affected = sqlx::query("DELETE FROM documents WHERE id = $1 AND status = 'deleting'")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        done += affected;
    }
    Ok(done)
}

/// Open a transaction bound to one tenant, so RLS confines every statement in it.
async fn tenant_tx<'a>(
    db: &'a PgPool,
    tenant_id: &str,
) -> anyhow::Result<sqlx::Transaction<'a, sqlx::Postgres>> {
    let mut tx = db.begin().await?;
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

async fn sweep_one(db: &PgPool, tenant_id: &str, sql: &str) -> anyhow::Result<u64> {
    let mut tx = db.begin().await?;
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;
    let affected = sqlx::query(sql).execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok(affected)
}
