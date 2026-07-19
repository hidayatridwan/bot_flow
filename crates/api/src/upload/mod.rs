//! Presigned upload sessions.
//!
//! The API never touches file bytes. It authenticates the tenant, records the document, and hands
//! back a URL the client PUTs to directly. MinIO announces the finished upload over AMQP, and the
//! worker takes it from there — there is no `/complete` callback to forget or forge.

pub use common::key;

use axum::http::StatusCode;
use s3::Bucket;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::db;
use crate::error::AppError;

pub struct UploadSession {
    pub document_id: Uuid,
    pub upload_url: String,
    pub expires_at: String,
}

/// Create a document row and mint a presigned PUT for it.
///
/// The row is written *before* the URL exists, so an object can never arrive without a row to
/// account for it. The reverse — a row with no object — is the abandoned-upload case the reaper
/// settles.
pub async fn create_session(
    db: &PgPool,
    s3_public: &Bucket,
    tenant_id: &str,
    filename: &str,
    ttl_secs: u32,
) -> Result<UploadSession, AppError> {
    let ext = checked_extension(filename)?;

    let document_id = Uuid::new_v4();
    let object_key = key::object_key(tenant_id, &document_id, &ext);
    let content_type = key::content_type_for(&ext);

    let mut tx = db::tenant_tx(db, tenant_id).await?;
    let row = sqlx::query(
        "INSERT INTO documents (id, tenant_id, filename, object_key, content_type, status, upload_expires_at)
         VALUES ($1, $2, $3, $4, $5, 'uploading', now() + make_interval(secs => $6))
         RETURNING upload_expires_at::text AS expires_at",
    )
    .bind(document_id)
    .bind(tenant_id)
    .bind(filename)
    .bind(&object_key)
    .bind(content_type)
    .bind(ttl_secs as f64)
    .fetch_one(&mut *tx)
    .await?;
    let expires_at: String = row.get("expires_at");
    tx.commit().await?;

    let upload_url = presign(s3_public, &object_key, ttl_secs).await?;
    Ok(UploadSession {
        document_id,
        upload_url,
        expires_at,
    })
}

/// Re-mint a URL for a document whose upload never landed. Without this an expired URL leaves a
/// permanently dead row: the object key is taken, but nothing can ever be written to it.
pub async fn refresh_session(
    db: &PgPool,
    s3_public: &Bucket,
    tenant_id: &str,
    document_id: Uuid,
    ttl_secs: u32,
) -> Result<UploadSession, AppError> {
    let mut tx = db::tenant_tx(db, tenant_id).await?;
    // RLS scopes this: another tenant's id is invisible, so it 404s exactly like a missing one.
    let row = sqlx::query(
        "UPDATE documents SET upload_expires_at = now() + make_interval(secs => $2), status = 'uploading'
         WHERE id = $1 AND status IN ('uploading', 'expired')
         RETURNING object_key, upload_expires_at::text AS expires_at",
    )
    .bind(document_id)
    .bind(ttl_secs as f64)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    let row = row.ok_or_else(|| {
        AppError::client(
            StatusCode::NOT_FOUND,
            "no document awaiting upload with that id",
        )
    })?;
    let object_key: String = row.get("object_key");
    let expires_at: String = row.get("expires_at");

    let upload_url = presign(s3_public, &object_key, ttl_secs).await?;
    Ok(UploadSession {
        document_id,
        upload_url,
        expires_at,
    })
}

/// Validate a filename and derive everything the object key needs from it.
///
/// Shared so the presigned path and the inline path (`POST /ingest`, phase 11) cannot diverge on
/// which extensions are allowed or what error a rejected one produces. `extension_of` is the only
/// filename validator in the system — it checks the extension and nothing else, which is safe
/// because the filename never enters the object key; the document's UUID does.
pub fn checked_extension(filename: &str) -> Result<String, AppError> {
    key::extension_of(filename).ok_or_else(|| {
        AppError::client(
            StatusCode::BAD_REQUEST,
            format!(
                "unsupported file type; expected one of {}",
                key::ALLOWED_EXTENSIONS.join(", ")
            ),
        )
    })
}

/// A document row for content the API is about to write itself, rather than presign for.
///
/// Returns `(document_id, object_key)`. Two cases, and the difference is the whole point of
/// `external_id`:
///
/// * **No match** (or no `external_id`) — mint a new document, exactly as [`create_session`] does.
/// * **A match** — reuse that row's `document_id` *and* `object_key`, so the caller's next write
///   lands on the same object. MinIO then reports a different etag and invariant 10 re-indexes it.
///   That is what makes re-syncing a source an overwrite rather than a duplicate, and it reuses the
///   fingerprint machinery instead of inventing a second idempotency mechanism.
///
/// `upload_expires_at` is left **NULL**: the bytes are already in hand, so there is no window to
/// expire. That is also what keeps the reaper away — its sweep keys on
/// `upload_expires_at < now() - grace`, and a NULL never satisfies a comparison.
pub async fn inline_document(
    db: &PgPool,
    tenant_id: &str,
    filename: &str,
    ext: &str,
    external_id: Option<&str>,
) -> Result<(Uuid, String), AppError> {
    let mut tx = db::tenant_tx(db, tenant_id).await?;

    if let Some(external_id) = external_id {
        // RLS scopes this to the tenant, and the unique index is (tenant_id, external_id), so a
        // match here can only ever be this tenant's own document.
        let existing = sqlx::query(
            "SELECT id, object_key FROM documents WHERE external_id = $1 AND status <> 'deleting'",
        )
        .bind(external_id)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(row) = existing {
            let document_id: Uuid = row.get("id");
            let object_key: String = row.get("object_key");
            // Back to `uploading`: the object is about to be replaced, and the worker must treat
            // what follows as a fresh index rather than a finished one.
            sqlx::query(
                "UPDATE documents SET status = 'uploading', filename = $2, error = null WHERE id = $1",
            )
            .bind(document_id)
            .bind(filename)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok((document_id, object_key));
        }
    }

    let document_id = Uuid::new_v4();
    let object_key = key::object_key(tenant_id, &document_id, ext);
    sqlx::query(
        "INSERT INTO documents (id, tenant_id, filename, object_key, content_type, status, external_id)
         VALUES ($1, $2, $3, $4, $5, 'uploading', $6)",
    )
    .bind(document_id)
    .bind(tenant_id)
    .bind(filename)
    .bind(&object_key)
    .bind(key::content_type_for(ext))
    .bind(external_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((document_id, object_key))
}

/// Sign a PUT for exactly this key. Purely local HMAC — no network call to MinIO.
///
/// The signature binds the method, the key and the expiry, and nothing else. It notably does
/// **not** bind the body length: a client holding this URL can upload a file of any size. The
/// worker enforces `MAX_UPLOAD_BYTES` after the fact, on the ObjectCreated event.
async fn presign(bucket: &Bucket, object_key: &str, ttl_secs: u32) -> Result<String, AppError> {
    bucket
        .presign_put(format!("/{object_key}"), ttl_secs, None, None)
        .await
        .map_err(|e| AppError::Internal(anyhow::Error::new(e).context("failed to presign upload")))
}
