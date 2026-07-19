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

    if let Some(skip) = claim_decision(&status, known_etag.as_deref(), etag) {
        return Ok(skip);
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

/// Given a document's current status, decide whether the worker should skip it rather than claim it.
/// `None` means proceed. Pure so the state machine can be tested without a database.
fn claim_decision(status: &str, known_etag: Option<&str>, etag: &str) -> Option<Claim> {
    match status {
        // Already indexed. If the etag matches, this is a duplicate event or a redelivery.
        // If it differs, the client overwrote the object and we must re-index it (proceed).
        "ready" if known_etag == Some(etag) => Some(Claim::Skip("already indexed, same etag")),
        // Another worker holds the lease. Its outcome governs; a crashed lease is reclaimed
        // by the reaper, not by us guessing here.
        "processing" => Some(Claim::Skip("another worker is processing it")),
        // Terminal and unretryable: the object violated policy.
        "quarantined" => Some(Claim::Skip("quarantined")),
        // Being erased (phase 8). A redelivered event must never re-claim a tombstoned row — that
        // would flip `deleting` back to `processing` and re-index a document mid-deletion. The delete
        // saga owns this row now; the worker's only correct move is to step away.
        "deleting" => Some(Claim::Skip("being deleted")),
        _ => None,
    }
}

/// Terminal success. Guarded on `processing` — see [`finish_from_processing`].
pub async fn mark_ready(db: &PgPool, tenant_id: &str, document_id: Uuid) -> anyhow::Result<bool> {
    finish_from_processing(db, tenant_id, document_id, "ready", None).await
}

/// Retryable failure. The queue's delivery limit decides when to stop trying. Guarded on
/// `processing` — see [`finish_from_processing`].
pub async fn mark_failed(
    db: &PgPool,
    tenant_id: &str,
    document_id: Uuid,
    error: &str,
) -> anyhow::Result<bool> {
    finish_from_processing(db, tenant_id, document_id, "failed", Some(error)).await
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

/// A worker's terminal transition **out of `processing`**, guarded so it only fires on a row the
/// worker still owns.
///
/// The guard is the phase-8 fence (invariant 10). The worker holds no DB lock while it indexes, so
/// between `claim` and here a delete may have tombstoned the row to `deleting`, or the reaper may
/// have reclaimed a stale lease to `failed`. In either case the row is no longer `processing`, the
/// `WHERE status = 'processing'` matches nothing, and we return `false` **without writing** — because
/// writing would resurrect a document being erased (the delete sweep looks for `deleting` and would
/// never find a row we flipped to `ready`/`failed`). Any chunks already upserted become orphans the
/// sweep clears by `document_id`.
///
/// Returns whether the row was ours to finish. `false` is a normal outcome, not an error.
async fn finish_from_processing(
    db: &PgPool,
    tenant_id: &str,
    document_id: Uuid,
    status: &str,
    error: Option<&str>,
) -> anyhow::Result<bool> {
    let mut tx = tenant_tx(db, tenant_id).await?;
    let result = sqlx::query(
        "UPDATE documents
            SET status = $2,
                error = $3,
                processed_at = case when $2 = 'ready' then now() else processed_at end,
                processing_started_at = null
          WHERE id = $1 AND status = 'processing'",
    )
    .bind(document_id)
    .bind(status)
    .bind(error)
    .execute(&mut *tx)
    .await
    .context("failed to update document status")?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}

/// An unguarded status write, for a transition that does **not** originate from `processing`.
/// Currently only `mark_quarantined`, which fires on the oversize path *before* the claim.
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

#[cfg(test)]
mod tests {
    use super::*;

    // The claim state machine, tested without a database. The DB-touching halves (`claim`'s lock,
    // the guarded UPDATE) are exercised live; this pins the pure decision, including the new fence.
    #[test]
    fn deleting_is_skipped_so_a_redelivery_cannot_resurrect_it() {
        assert_eq!(
            claim_decision("deleting", None, "etag-1"),
            Some(Claim::Skip("being deleted"))
        );
    }

    #[test]
    fn a_fresh_or_reclaimed_row_proceeds() {
        // `failed` (reaper-reclaimed) and `expired` are re-claimable; a brand-new row has no etag.
        assert_eq!(claim_decision("failed", None, "etag-1"), None);
        assert_eq!(claim_decision("expired", None, "etag-1"), None);
        assert_eq!(claim_decision("uploading", None, "etag-1"), None);
    }

    #[test]
    fn ready_skips_only_when_the_etag_matches() {
        // Same bytes → duplicate/redelivery, skip. Different bytes → the client overwrote it, re-index.
        assert_eq!(
            claim_decision("ready", Some("etag-1"), "etag-1"),
            Some(Claim::Skip("already indexed, same etag"))
        );
        assert_eq!(claim_decision("ready", Some("etag-1"), "etag-2"), None);
    }

    #[test]
    fn processing_and_quarantined_are_skipped() {
        assert_eq!(
            claim_decision("processing", None, "e"),
            Some(Claim::Skip("another worker is processing it"))
        );
        assert_eq!(
            claim_decision("quarantined", None, "e"),
            Some(Claim::Skip("quarantined"))
        );
    }
}

/// Integration tests (phase 9b) — the halves the pure tests above cannot reach: the row lock, and
/// the `WHERE status = 'processing'` fence. `#[ignore]`d; see `testsupport` for why they live here.
#[cfg(test)]
mod integration {
    use super::*;
    use crate::testsupport::{seed_document, seed_tenant, status_of, test_pool};

    /// **Invariant 10's entire deduplication story, executed concurrently for the first time.**
    ///
    /// A row lock plus a status check is all that stops two workers indexing one document twice —
    /// and until now it had only ever been reasoned about. Two `claim` calls race the same row:
    /// exactly one must `Proceed`, and the loser must see `processing` and skip.
    ///
    /// Looped, because a race that fails one time in five is worse than no test at all: a single
    /// pass would let an interleaving that only sometimes goes wrong look permanently fine.
    /// (Verified: with `FOR UPDATE` removed this fails at round 1, three runs out of three.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs docker compose services + the bot_flow_test database"]
    async fn two_workers_racing_one_document_produce_exactly_one_proceed() {
        let db = test_pool().await;
        let tenant = seed_tenant(&db).await;

        for round in 0..10 {
            let doc = seed_document(&db, &tenant, "uploading", None).await;

            // Same arguments both sides: a duplicate MinIO delivery, not two different uploads.
            let (a, b) = tokio::join!(
                claim(&db, &tenant, doc, 1234, "etag-same"),
                claim(&db, &tenant, doc, 1234, "etag-same"),
            );
            let (a, b) = (a.expect("claim A errored"), b.expect("claim B errored"));

            let proceeds = [&a, &b].iter().filter(|c| ***c == Claim::Proceed).count();
            assert_eq!(
                proceeds, 1,
                "round {round}: expected exactly one Proceed, got {a:?} and {b:?}. \
                 Two Proceeds means the row lock is not serialising the claim and the document \
                 would be indexed twice; zero means neither worker took it and it stalls forever."
            );

            // The loser must skip for the right reason — it saw the winner's `processing`, not
            // some other branch that happens to also skip.
            let loser = if a == Claim::Proceed { &b } else { &a };
            assert_eq!(
                *loser,
                Claim::Skip("another worker is processing it"),
                "round {round}: the losing worker skipped for the wrong reason"
            );

            assert_eq!(
                status_of(&db, &tenant, doc).await.as_deref(),
                Some("processing"),
                "round {round}: the winner did not leave the row in processing"
            );
        }
    }

    /// The phase-8 fence (invariant 10): a worker that finishes late must not resurrect a row it no
    /// longer owns. `mark_ready` fires only `WHERE status = 'processing'`.
    ///
    /// Without the guard an unguarded `mark_ready` flips `deleting` back to `ready` — and the delete
    /// sweep, which looks for `deleting`, would then never find it again. The document is erased from
    /// the tenant's listing but answers searches forever.
    #[tokio::test]
    #[ignore = "needs docker compose services + the bot_flow_test database"]
    async fn a_late_worker_cannot_resurrect_a_tombstoned_document() {
        let db = test_pool().await;
        let tenant = seed_tenant(&db).await;

        // A delete landed mid-index: the row is tombstoned while our worker was still writing.
        let doc = seed_document(&db, &tenant, "deleting", Some("1 minute")).await;

        let finished = mark_ready(&db, &tenant, doc)
            .await
            .expect("mark_ready errored");

        assert!(
            !finished,
            "mark_ready reported it finished a row it no longer owns — the \
             `WHERE status = 'processing'` fence is gone"
        );
        assert_eq!(
            status_of(&db, &tenant, doc).await.as_deref(),
            Some("deleting"),
            "RESURRECTION: a late worker flipped a tombstoned row out of `deleting`. The delete \
             sweep looks for `deleting` and will now never find it — the document is gone from the \
             tenant's listing but still answers searches."
        );

        // Same fence on the failure path.
        let doc2 = seed_document(&db, &tenant, "deleting", Some("1 minute")).await;
        let finished = mark_failed(&db, &tenant, doc2, "boom")
            .await
            .expect("mark_failed errored");
        assert!(!finished, "mark_failed is not guarded on `processing`");
        assert_eq!(
            status_of(&db, &tenant, doc2).await.as_deref(),
            Some("deleting")
        );
    }

    /// A document belonging to another tenant is invisible, not forbidden — RLS hides the row, so
    /// `claim` takes its "no such document" branch rather than seeing someone else's work.
    #[tokio::test]
    #[ignore = "needs docker compose services + the bot_flow_test database"]
    async fn a_foreign_document_is_invisible_to_the_claim() {
        let db = test_pool().await;
        let owner = seed_tenant(&db).await;
        let stranger = seed_tenant(&db).await;
        let doc = seed_document(&db, &owner, "uploading", None).await;

        let claimed = claim(&db, &stranger, doc, 1, "etag")
            .await
            .expect("claim errored");

        assert_eq!(
            claimed,
            Claim::Skip("no such document for this tenant"),
            "TENANCY LEAK: a worker running as one tenant could see another tenant's document row"
        );
        // And the owner's row is untouched by the stranger's attempt.
        assert_eq!(
            status_of(&db, &owner, doc).await.as_deref(),
            Some("uploading")
        );
    }
}
