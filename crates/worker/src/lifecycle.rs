//! Document status transitions.
//!
//! Every transition is guarded by the row's current state, under `SELECT … FOR UPDATE`. That lock
//! is what makes the worker idempotent: MinIO can deliver an event twice, RabbitMQ can redeliver
//! it, and two workers can race for the same document — only one does the work.
//!
//! All statements run inside a transaction that has set `app.current_tenant`, so RLS confines
//! them to one tenant. A row belonging to someone else is invisible, not forbidden.

use anyhow::Context;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// What the worker should do with an incoming event.
#[derive(Debug, PartialEq)]
pub enum Claim {
    /// We own this document; go index it.
    Proceed,
    /// Someone already did this work, or is doing it. Ack and move on.
    Skip(&'static str),
}

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

/// Try to move a document into `processing`, recording what MinIO told us about the object.
///
/// The row lock plus the status check is the entire deduplication story. Every branch below is a
/// real event we have to handle, not defensive padding.
pub async fn claim(
    db: &PgPool,
    tenant_id: &str,
    document_id: Uuid,
    size: i64,
    etag: &str,
) -> anyhow::Result<Claim> {
    let mut tx = tenant_tx(db, tenant_id).await?;

    let row = sqlx::query("SELECT status, etag FROM documents WHERE id = $1 FOR UPDATE")
        .bind(document_id)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to load document")?;

    // No row: either the id never existed, or it belongs to another tenant and RLS is hiding it.
    // An object cannot exist without a row (the row is written before the URL is minted), so this
    // means someone deleted it. Nothing to do.
    let Some(row) = row else {
        return Ok(Claim::Skip("no such document for this tenant"));
    };

    let status: String = row.get("status");
    let known_etag: Option<String> = row.get("etag");

    match status.as_str() {
        // Already indexed. If the etag matches, this is a duplicate event or a redelivery.
        // If it differs, the client overwrote the object and we must re-index it.
        "ready" if known_etag.as_deref() == Some(etag) => {
            return Ok(Claim::Skip("already indexed, same etag"));
        }
        // Another worker holds the lease. Its outcome governs; a crashed lease is reclaimed
        // by the reaper, not by us guessing here.
        "processing" => return Ok(Claim::Skip("another worker is processing it")),
        // Terminal and unretryable: the object violated policy.
        "quarantined" => return Ok(Claim::Skip("quarantined")),
        _ => {}
    }

    sqlx::query(
        "UPDATE documents
            SET status = 'processing',
                size_bytes = $2,
                etag = $3,
                uploaded_at = coalesce(uploaded_at, now()),
                processing_started_at = now(),
                attempts = attempts + 1,
                error = null
          WHERE id = $1",
    )
    .bind(document_id)
    .bind(size)
    .bind(etag)
    .execute(&mut *tx)
    .await
    .context("failed to claim document")?;

    tx.commit().await?;
    Ok(Claim::Proceed)
}

/// Terminal success.
pub async fn mark_ready(db: &PgPool, tenant_id: &str, document_id: Uuid) -> anyhow::Result<()> {
    set_status(db, tenant_id, document_id, "ready", None).await
}

/// Retryable failure. The queue's delivery limit decides when to stop trying.
pub async fn mark_failed(
    db: &PgPool,
    tenant_id: &str,
    document_id: Uuid,
    error: &str,
) -> anyhow::Result<()> {
    set_status(db, tenant_id, document_id, "failed", Some(error)).await
}

/// The object broke a rule no retry can fix (oversize, unreadable type). The bytes are deleted.
pub async fn mark_quarantined(
    db: &PgPool,
    tenant_id: &str,
    document_id: Uuid,
    reason: &str,
) -> anyhow::Result<()> {
    set_status(db, tenant_id, document_id, "quarantined", Some(reason)).await
}

async fn set_status(
    db: &PgPool,
    tenant_id: &str,
    document_id: Uuid,
    status: &str,
    error: Option<&str>,
) -> anyhow::Result<()> {
    let mut tx = tenant_tx(db, tenant_id).await?;
    let result = sqlx::query(
        "UPDATE documents
            SET status = $2,
                error = $3,
                processed_at = case when $2 = 'ready' then now() else processed_at end,
                processing_started_at = null
          WHERE id = $1",
    )
    .bind(document_id)
    .bind(status)
    .bind(error)
    .execute(&mut *tx)
    .await
    .context("failed to update document status")?;
    tx.commit().await?;

    // If RLS or the tenant id is wrong this silently matches nothing — surface it rather than
    // let the document sit in `processing` forever.
    if result.rows_affected() == 0 {
        anyhow::bail!("status update matched 0 rows (RLS/tenant mismatch?) for {document_id}");
    }
    Ok(())
}
