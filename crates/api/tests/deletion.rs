//! The deletion saga's API half (phase 9b).
//!
//! `DELETE /documents/{id}` erases a document across Postgres, Qdrant and MinIO with no transaction
//! spanning the three. What makes that safe is the tombstone: the row moves to `deleting` first, on
//! its own, which drops it from the tenant's listing immediately and fences out a worker that is
//! still indexing (invariant 10). Everything after is idempotent cleanup a crash can resume.
//!
//! The reaper half — which `deleting` rows are safe to finish, and why a live lease must be waited
//! out — is in `crates/worker/src/reaper.rs`, next to the private seams it drives.

mod common;

use axum::body::Body;
use common::{json_request, TestApp};
use sqlx::PgPool;

/// Force a document into a given state, under tenant context because `documents` has RLS.
///
/// Reaching past the API is deliberate here and only here: the `202` branch requires a row that a
/// worker is actively indexing, and there is no endpoint that produces one — it is a state the
/// worker owns. Everything else in this file goes through HTTP.
async fn force_processing(db: &PgPool, tenant_id: &str, document_id: &str) {
    let mut tx = db.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let affected = sqlx::query(
        "UPDATE documents SET status = 'processing', processing_started_at = now() WHERE id = $1",
    )
    .bind(uuid::Uuid::parse_str(document_id).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap()
    .rows_affected();
    tx.commit().await.unwrap();
    // Under RLS a wrong tenant matches zero rows and reports success — the corollary trap. If the
    // setup silently did nothing, the assertions below would be testing the wrong thing.
    assert_eq!(affected, 1, "test setup failed to force `processing`");
}

async fn status_of(db: &PgPool, tenant_id: &str, document_id: &str) -> Option<String> {
    use sqlx::Row;
    let mut tx = db.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let row = sqlx::query("SELECT status FROM documents WHERE id = $1")
        .bind(uuid::Uuid::parse_str(document_id).unwrap())
        .fetch_optional(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    row.map(|r| r.get("status"))
}

async fn new_document(app: &TestApp, key: &str) -> String {
    let (status, body) = app
        .request(
            json_request("POST", "/documents/upload-url", key)
                .body(Body::from(r#"{"filename":"doomed.txt"}"#))
                .unwrap(),
        )
        .await;
    assert_eq!(status, 201, "upload-url failed: {body}");
    body["document_id"].as_str().unwrap().to_string()
}

/// No worker involved: erase inline, report `204`, and the row is gone.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn deleting_an_idle_document_is_synchronous_and_idempotent() {
    let app = TestApp::new().await;
    let (tenant, key) = app.create_tenant().await;
    let doc = new_document(&app, &key).await;

    let (status, _) = app
        .request(
            json_request("DELETE", &format!("/documents/{doc}"), &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, 204, "an idle document should be erased inline");
    assert_eq!(
        status_of(&app.db, &tenant, &doc).await,
        None,
        "the row survived a 204"
    );

    // A repeat delete is an unknown id now, and an unknown id is a 404 — never a 500, and never an
    // oracle distinguishing "never existed" from "someone else's" (invariant 8).
    let (status, _) = app
        .request(
            json_request("DELETE", &format!("/documents/{doc}"), &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, 404, "a second delete must 404, not error");
}

/// A delete landing *while a worker indexes* must tombstone and defer, not erase.
///
/// Deleting the vectors now would race the worker's in-flight upsert and lose — the chunks would
/// land after the delete and answer searches for a document that no longer exists. So: `202`, the
/// row goes to `deleting`, and the reaper finishes it once the lease has provably elapsed.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn deleting_a_document_mid_index_tombstones_and_defers() {
    let app = TestApp::new().await;
    let (tenant, key) = app.create_tenant().await;
    let doc = new_document(&app, &key).await;
    force_processing(&app.db, &tenant, &doc).await;

    let (status, _) = app
        .request(
            json_request("DELETE", &format!("/documents/{doc}"), &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        status, 202,
        "a delete racing an active index must defer, not erase"
    );

    assert_eq!(
        status_of(&app.db, &tenant, &doc).await.as_deref(),
        Some("deleting"),
        "the row was not tombstoned — nothing now fences the worker out, and an unguarded \
         mark_ready would resurrect a document being erased"
    );

    // Gone from the tenant's listing immediately: as far as they are concerned it has left, even
    // though the stores are cleaned up later.
    let (status, body) = app
        .request(
            json_request("GET", "/documents", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, 200);
    let listed: Vec<&str> = body["documents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["id"].as_str())
        .collect();
    assert!(
        !listed.contains(&doc.as_str()),
        "a `deleting` document is still listed — the tenant asked for it to be erased and can \
         still see it"
    );

    // Deleting again is not an error: the saga is resumable by design.
    let (status, _) = app
        .request(
            json_request("DELETE", &format!("/documents/{doc}"), &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert!(
        status == 202 || status == 404,
        "a repeat delete on a tombstoned row must not 500; got {status}"
    );

    app.cleanup().await;
}
