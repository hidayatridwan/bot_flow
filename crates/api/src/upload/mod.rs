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
    let ext = key::extension_of(filename).ok_or_else(|| {
        AppError::client(
            StatusCode::BAD_REQUEST,
            format!(
                "unsupported file type; expected one of {}",
                key::ALLOWED_EXTENSIONS.join(", ")
            ),
        )
    })?;

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
