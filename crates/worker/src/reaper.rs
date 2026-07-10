//! Settles documents that no event will ever arrive for.
//!
//! Two kinds of stuck row:
//!   * `uploading` whose presigned URL lapsed — the client never PUT anything.
//!   * `processing` whose worker died mid-job — the lease is stale and nobody will finish it.
//!
//! Note the per-tenant loop. `documents` has FORCE ROW LEVEL SECURITY and the worker connects as
//! the non-superuser `app_user`, so a single cross-tenant UPDATE would match zero rows and appear
//! to succeed. The policy compares against `app.current_tenant`, which we must set per tenant.

use sqlx::{PgPool, Row};
use std::time::Duration;

/// Absorbs clock skew and slow uploads: a signature is checked when the PUT *starts*, so a
/// transfer that began just before expiry is legitimate and may still be in flight.
const UPLOAD_GRACE: &str = "5 minutes";

/// How long a worker may hold a document before we assume it died.
const PROCESSING_LEASE: &str = "30 minutes";

pub fn spawn(db: PgPool, every: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        loop {
            ticker.tick().await;
            if let Err(e) = sweep(&db).await {
                tracing::error!("reaper sweep failed: {e:#}");
            }
        }
    });
}

async fn sweep(db: &PgPool) -> anyhow::Result<()> {
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

        if expired > 0 || reclaimed > 0 {
            tracing::info!(
                tenant = %tenant_id,
                "reaper: {expired} abandoned upload(s) expired, {reclaimed} stale lease(s) reclaimed"
            );
        }
    }
    Ok(())
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
