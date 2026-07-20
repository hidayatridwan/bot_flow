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

/// Reclaim documents whose worker died holding the lease.
///
/// A function rather than an inline `format!` because the test drives *this* statement. It used to
/// hold a verbatim copy, so the two could disagree — and a test asserting on its own private copy
/// of the SQL passes no matter what the reaper actually does.
///
/// Back to `failed`, not `uploaded`: the object is still there, so a redelivered event (or a manual
/// retry) can claim it again. `failed` is an accepted claim source.
///
/// The reason is `system_error` unconditionally, and that is the point of writing it here: a lease
/// expires because *our* worker died, so the tenant's file may be perfectly good. This is precisely
/// the case that made a `failed` badge unactionable — in the `error` column it is indistinguishable
/// from a corrupt upload, and the classified column is what tells the tenant to wait rather than
/// re-upload a file that was never broken.
fn reclaim_stale_leases_sql() -> String {
    format!(
        "UPDATE documents
            SET status = 'failed',
                error = 'processing lease expired; worker presumed dead',
                failure_reason = 'system_error',
                processing_started_at = null
          WHERE status = 'processing'
            AND processing_started_at < now() - interval '{PROCESSING_LEASE}'"
    )
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

        let reclaimed = sweep_one(db, &tenant_id, &reclaim_stale_leases_sql()).await?;

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

/// Integration tests (phase 9b). `#[ignore]`d; see `testsupport` for why they are in-crate.
///
/// These drive `finish_deletions` and the reclaim sweep directly, which is the reason this module
/// has no `lib.rs`: both, plus `PROCESSING_LEASE`, are private, and making them `pub` would buy
/// nothing but the test.
#[cfg(test)]
mod integration {
    use super::*;
    use crate::testsupport::{failure_reason_of, seed_document, seed_tenant, status_of, test_pool};

    /// **The case the phase-9 design says took a live stack to check.**
    ///
    /// A delete that lands while a worker is indexing returns `202` and defers cleanup to this
    /// sweep — which must not erase the vectors until the worker has provably released, i.e. one
    /// `PROCESSING_LEASE` after indexing began. The worker holds no DB lock while it indexes, so
    /// the lease is the *only* thing standing between the sweep and a delete racing an active
    /// upsert. Erase early and the worker's in-flight chunks land after the delete: orphaned
    /// vectors, answering searches for a document that no longer exists.
    ///
    /// Both directions are asserted, because only checking the erase half would pass on a sweep
    /// that ignored the lease entirely — which is precisely the bug.
    #[tokio::test]
    #[ignore = "needs docker compose services + the bot_flow_test database"]
    async fn the_delete_sweep_waits_out_a_live_lease_but_not_an_expired_one() {
        let db = test_pool().await;
        let qdrant = Qdrant::from_url(&std::env::var("QDRANT_URL").expect("QDRANT_URL is not set"))
            .build()
            .expect("failed to build Qdrant client");
        let bucket = crate::build_bucket().expect("failed to build S3 bucket");
        let tenant = seed_tenant(&db).await;

        // Tombstoned one minute ago, while a worker was indexing: the lease is still live.
        let live = seed_document(&db, &tenant, "deleting", Some("1 minute")).await;
        // Tombstoned long enough ago that the worker is provably done or dead.
        let stale = seed_document(&db, &tenant, "deleting", Some("31 minutes")).await;
        // Never touched by a worker at all — nothing to wait for, safe immediately.
        let untouched = seed_document(&db, &tenant, "deleting", None).await;

        let finished = finish_deletions(&db, &qdrant, &bucket, &tenant)
            .await
            .expect("finish_deletions errored");

        assert_eq!(
            status_of(&db, &tenant, live).await.as_deref(),
            Some("deleting"),
            "the sweep erased a row whose worker may still be writing vectors. The lease has not \
             elapsed, so an in-flight upsert can still land after this delete and strand orphaned \
             vectors that answer searches for an erased document."
        );
        assert_eq!(
            status_of(&db, &tenant, stale).await,
            None,
            "the sweep left a row whose lease expired {PROCESSING_LEASE} ago — a deferred delete \
             that never completes is a document the tenant asked us to erase and we did not"
        );
        assert_eq!(
            status_of(&db, &tenant, untouched).await,
            None,
            "a tombstoned row with no processing_started_at has no worker to wait for and must be \
             erased on the first sweep"
        );
        assert_eq!(
            finished, 2,
            "expected exactly the two safe rows to be erased"
        );
    }

    /// The reclaim sweep: a worker that died mid-index leaves `processing` forever, because nothing
    /// else ever moves that row. Back to `failed` rather than `uploaded` — the object is still
    /// there, so `failed` is a re-claimable state and a redelivery can pick it up.
    #[tokio::test]
    #[ignore = "needs docker compose services + the bot_flow_test database"]
    async fn a_stale_lease_is_reclaimed_and_a_live_one_is_left_alone() {
        let db = test_pool().await;
        let tenant = seed_tenant(&db).await;

        let live = seed_document(&db, &tenant, "processing", Some("1 minute")).await;
        let dead = seed_document(&db, &tenant, "processing", Some("31 minutes")).await;

        let reclaimed = sweep_one(&db, &tenant, &reclaim_stale_leases_sql())
            .await
            .expect("sweep_one errored");

        assert_eq!(
            reclaimed, 1,
            "expected exactly the dead lease to be reclaimed"
        );
        assert_eq!(
            status_of(&db, &tenant, dead).await.as_deref(),
            Some("failed"),
            "a worker presumed dead must release its document, or it is stuck in `processing` forever"
        );
        assert_eq!(
            status_of(&db, &tenant, live).await.as_deref(),
            Some("processing"),
            "the reaper reclaimed a lease that had not expired — it would be racing a live worker, \
             and `mark_ready`'s fence would then silently drop that worker's finished index"
        );

        assert_eq!(
            failure_reason_of(&db, &tenant, dead).await.as_deref(),
            Some("system_error"),
            "a dead worker is our fault, not the document's — classifying it any other way tells \
             the tenant to re-upload a file that was never broken"
        );
    }

    /// The corollary trap, asserted rather than trusted: a sweep run under one tenant's context
    /// must not touch another tenant's rows — and under RLS the failure mode is *silent success*,
    /// zero rows matched and no error.
    #[tokio::test]
    #[ignore = "needs docker compose services + the bot_flow_test database"]
    async fn a_sweep_cannot_reach_another_tenants_rows() {
        let db = test_pool().await;
        let owner = seed_tenant(&db).await;
        let stranger = seed_tenant(&db).await;
        let doc = seed_document(&db, &owner, "processing", Some("31 minutes")).await;

        // Same sweep, wrong tenant. It should match nothing at all.
        let reclaimed = sweep_one(
            &db,
            &stranger,
            &format!(
                "UPDATE documents SET status = 'failed'
                  WHERE status = 'processing'
                    AND processing_started_at < now() - interval '{PROCESSING_LEASE}'"
            ),
        )
        .await
        .expect("sweep_one errored");

        assert_eq!(
            reclaimed, 0,
            "TENANCY LEAK: a sweep bound to one tenant updated another tenant's document"
        );
        assert_eq!(
            status_of(&db, &owner, doc).await.as_deref(),
            Some("processing"),
            "the owner's row was modified by a sweep running as a different tenant"
        );
    }
}
