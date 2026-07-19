//! Erasing data, and recording that it happened.
//!
//! Phase 11 made a *document* erasable. This makes a *tenant* erasable, and gives both an audit
//! record — because "we deleted it" is a claim, and a compliance question is a request for evidence.
//!
//! **The audit row deliberately outlives its subject.** `erasures` has no foreign key to `tenants`,
//! so deleting a tenant cascades through every other table and leaves the record of that deletion
//! standing. An erasure log destroyed by the erasure it documents would be worse than none: it would
//! look like diligence.

use anyhow::Context;
use qdrant_client::qdrant::{Condition, DeletePointsBuilder, Filter};
use s3::Bucket;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;
use common::COLLECTION;

/// Who asked for the erasure. Not *which credential* — invariant 14 keeps keys out of every log,
/// and storing a hash would be a fingerprint linking rows to a key we could not otherwise name.
pub fn actor_label(kind: &crate::auth::ActorKind) -> &'static str {
    match kind {
        crate::auth::ActorKind::Secret => "secret",
        crate::auth::ActorKind::Session => "session",
        crate::auth::ActorKind::Publishable => "publishable",
    }
}

/// Open an audit row. Returns its id, to be closed by [`finish`] when the stores are actually clear.
///
/// Written on the **plain pool**, not `tenant_tx`: `erasures` has no RLS and, for a tenant erasure,
/// there will shortly be no tenant to scope it to.
pub async fn begin(
    db: &PgPool,
    tenant_id: &str,
    scope: &str,
    document_id: Option<Uuid>,
    actor: &str,
) -> Result<Uuid, AppError> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO erasures (id, tenant_id, scope, document_id, actor)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(scope)
    .bind(document_id)
    .bind(actor)
    .execute(db)
    .await?;
    Ok(id)
}

/// Close an audit row with what was actually removed.
///
/// A row left open is an erasure that started and did not finish — which is the case worth finding,
/// and the reason `completed_at` is separate from `requested_at` rather than one timestamp.
pub async fn finish(
    db: &PgPool,
    id: Uuid,
    vectors: Option<u64>,
    objects: Option<u64>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE erasures SET completed_at = now(), vectors_deleted = $2, objects_deleted = $3
         WHERE id = $1",
    )
    .bind(id)
    .bind(vectors.map(|v| v as i64))
    .bind(objects.map(|v| v as i64))
    .execute(db)
    .await?;
    Ok(())
}

/// Erase everything belonging to one tenant, across all three stores.
///
/// **The order is the same as the document saga's, and for the same reason:** vectors, then objects,
/// then rows. A crash partway leaves the least-bad orphan — surviving *rows* are inert, while
/// surviving *vectors* would still answer questions.
///
/// **Access is revoked first**, before anything is deleted. Keys and sessions go in their own
/// committed transaction, so from that moment no request can authenticate as this tenant and no new
/// work can begin. Without it, a client could mint an upload URL into a tenant being erased.
///
/// **The vector sweep runs twice.** A worker that was already mid-index when we started holds no
/// database lock (invariant 10) and can upsert chunks after the first sweep. Its `mark_ready` will
/// then find no row and stop — correctly — but the chunks it wrote are already in the collection.
/// The second sweep, after the rows are gone, catches exactly that. It is one filtered call, and it
/// converts a race into a bounded one.
pub async fn erase_tenant(state: &AppState, tenant_id: &str) -> Result<(u64, u64), AppError> {
    // 1. Revoke access. Its own transaction: it must be durable before anything else happens.
    sqlx::query("DELETE FROM api_keys WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&state.db)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&state.db)
        .await?;
    tracing::info!(tenant = %tenant_id, "erasure: access revoked");

    // 2. Vectors.
    let vectors = delete_tenant_vectors(state, tenant_id).await?;

    // 3. Objects. Every key this tenant owns lives under one prefix — which is exactly why the
    //    tenant slug is constrained by both a regex and a DB CHECK (invariant 3): a tenant named
    //    `a/../b` could reach outside its own prefix, and here that would mean erasing a neighbour.
    let objects = delete_tenant_objects(&state.s3, tenant_id).await?;

    // 4. Rows. One DELETE; the FK cascade takes documents, conversations, messages, accounts and
    //    what remains of api_keys/sessions with it. `erasures` has no FK, so the audit survives.
    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&state.db)
        .await?;

    // 5. The second sweep — see the doc comment.
    let late = delete_tenant_vectors(state, tenant_id).await?;
    if late > 0 {
        tracing::warn!(
            tenant = %tenant_id,
            "erasure: {late} vector(s) arrived after the first sweep and were removed"
        );
    }

    Ok((vectors + late, objects))
}

/// Delete every point carrying this tenant's id. One filtered call.
async fn delete_tenant_vectors(state: &AppState, tenant_id: &str) -> Result<u64, AppError> {
    let before = count_tenant_vectors(state, tenant_id).await?;
    if before == 0 {
        return Ok(0);
    }
    state
        .qdrant
        .delete_points(
            DeletePointsBuilder::new(COLLECTION)
                .points(Filter::must([Condition::matches(
                    "tenant_id",
                    tenant_id.to_string(),
                )]))
                .wait(true),
        )
        .await
        .context("failed to delete tenant vectors")?;
    Ok(before)
}

async fn count_tenant_vectors(state: &AppState, tenant_id: &str) -> Result<u64, AppError> {
    let count = state
        .qdrant
        .count(
            qdrant_client::qdrant::CountPointsBuilder::new(COLLECTION)
                .filter(Filter::must([Condition::matches(
                    "tenant_id",
                    tenant_id.to_string(),
                )]))
                .exact(true),
        )
        .await
        .context("failed to count tenant vectors")?;
    Ok(count.result.map(|r| r.count).unwrap_or(0))
}

/// Delete every object under `tenants/{tenant_id}/`.
///
/// Listed then deleted rather than assumed: a document row is not the only thing that can put an
/// object there (a re-minted upload, a legacy path), and an erasure that only removes what the
/// database remembers is an erasure with a blind spot.
async fn delete_tenant_objects(bucket: &Bucket, tenant_id: &str) -> Result<u64, AppError> {
    let prefix = format!("tenants/{tenant_id}/");
    let pages = bucket
        .list(prefix, None)
        .await
        .map_err(|e| AppError::Internal(anyhow::Error::new(e).context("failed to list objects")))?;

    let mut deleted = 0u64;
    for page in pages {
        for object in page.contents {
            bucket.delete_object(&object.key).await.map_err(|e| {
                AppError::Internal(anyhow::Error::new(e).context("failed to delete object"))
            })?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// Redact conversation turns that quoted a document being erased.
///
/// **`messages` never stored the passages** — only the question and the model's answer. But an
/// answer is *derived* from the passages and routinely quotes them, so deleting a document while
/// leaving the answers that recite it is an erasure with a hole in it. Nothing linked the two until
/// assistant messages started carrying their sources in `metadata` (phase 12).
///
/// Redaction, not deletion: removing the row would renumber a conversation and leave the user's own
/// question answering itself. The turn stays, its content becomes a tombstone, and the provenance is
/// cleared so a second erasure of a different source cannot re-redact it.
///
/// Returns how many turns were redacted. Messages written before phase 12 carry no provenance and
/// cannot be found — a limit stated in the phase doc rather than hidden here.
pub async fn redact_messages_citing(
    db: &PgPool,
    tenant_id: &str,
    document_id: Uuid,
) -> Result<u64, AppError> {
    let mut tx = crate::db::tenant_tx(db, tenant_id).await?;
    let affected = sqlx::query(
        "UPDATE messages
            SET content = '[redacted: the source document was deleted]',
                metadata = metadata - 'document_ids'
          WHERE metadata -> 'document_ids' @> $1::jsonb",
    )
    .bind(serde_json::json!([document_id.to_string()]))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(affected)
}
