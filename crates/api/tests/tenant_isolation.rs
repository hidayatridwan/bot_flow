//! **The reason this phase exists.**
//!
//! Invariant 1 says every Qdrant search is filtered by tenant; invariant 2 says every row query
//! goes through `tenant_tx`. One forgotten filter in one handler leaks another company's support
//! documents, and until now both rested entirely on code review.
//!
//! Two legs, because there are two independent mechanisms and either could fail alone: Postgres RLS
//! (isolation layer 2) and the Qdrant payload filter (layer 1).
//!
//! Every test here runs as `app_user` — the harness asserts it and aborts otherwise. A superuser
//! bypasses RLS, so on the wrong credential all of this passes while testing nothing.

mod common;

use axum::body::Body;
use common::{json_request, TestApp};

/// Layer 2 — Postgres RLS.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn one_tenant_cannot_read_or_delete_anothers_document() {
    let app = TestApp::new().await;
    let (_a_id, a_key) = app.create_tenant().await;
    let (_b_id, b_key) = app.create_tenant().await;

    // A creates a document (a row exists before any URL — invariant 12).
    let (status, body) = app
        .request(
            json_request("POST", "/documents/upload-url", &a_key)
                .body(Body::from(r#"{"filename":"handbook.pdf"}"#.to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(status, 201, "upload-url failed: {body}");
    let a_doc = body["document_id"]
        .as_str()
        .expect("no document_id")
        .to_string();

    // B must not see it.
    let (status, body) = app
        .request(
            json_request("GET", "/documents", &b_key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, 200);
    let b_sees: Vec<&str> = body["documents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["id"].as_str())
        .collect();
    assert!(
        !b_sees.contains(&a_doc.as_str()),
        "TENANCY LEAK: tenant B's document list contains tenant A's document {a_doc}"
    );

    // B must not delete it — and must get 404, not 403. A 403 would make the endpoint an oracle
    // for which document ids exist (invariant 8's non-oracle rule).
    let (status, _) = app
        .request(
            json_request("DELETE", &format!("/documents/{a_doc}"), &b_key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        status, 404,
        "another tenant's document must 404, not {status} — anything else is an existence oracle"
    );

    // THE CONTROL, and the half a naive test forgets: A's document must still be there. Without
    // this, a `tenant_tx` that silently deleted everything would pass every assertion above.
    let (status, body) = app
        .request(
            json_request("GET", "/documents", &a_key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, 200);
    let a_sees: Vec<&str> = body["documents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["id"].as_str())
        .collect();
    assert!(
        a_sees.contains(&a_doc.as_str()),
        "tenant A can no longer see its own document {a_doc} — isolation is not the same as erasure"
    );

    // And A can delete its own.
    let (status, _) = app
        .request(
            json_request("DELETE", &format!("/documents/{a_doc}"), &a_key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        status, 204,
        "tenant A must be able to delete its own document"
    );

    app.cleanup().await;
}

/// Layer 1 — the Qdrant tenant filter.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn one_tenant_cannot_search_anothers_vectors() {
    let app = TestApp::new().await;
    let (a_id, a_key) = app.create_tenant().await;
    let (_b_id, b_key) = app.create_tenant().await;

    // A nonce, so this assertion cannot be satisfied by leftover data in the shared collection.
    let nonce = format!("nonce-{}", uuid::Uuid::new_v4().simple());
    let passage = format!("{nonce} refunds are accepted within thirty days of purchase");

    // Planted directly rather than through `/ingest`. Since phase 11 that route stores an object
    // and lets the worker index it, and the harness runs no worker — but the better reason is that
    // this test is about the **Qdrant tenant filter**, and it should fail for that reason or not at
    // all. Depending on the ingestion path made it a test of two things.
    app.plant_chunk(&a_id, &passage).await;

    // THE CONTROL, FIRST. If A cannot find its own passage, the denial assertion below is
    // worthless — a broken stub or a mis-set score floor makes *everyone* see nothing, and the test
    // would pass for entirely the wrong reason.
    let (status, body) = app.search(&a_key, &passage).await;
    assert_eq!(status, 200, "search failed: {body}");
    let a_hits = body["hits"].as_array().expect("no hits array");
    assert!(
        a_hits
            .iter()
            .any(|h| h["text"].as_str().unwrap_or("").contains(&nonce)),
        "tenant A cannot retrieve its own passage — the control failed, so the denial below \
         would prove nothing. Body: {body}"
    );

    // The assertion this file exists for.
    let (status, body) = app.search(&b_key, &passage).await;
    assert_eq!(status, 200);
    let b_hits = body["hits"].as_array().expect("no hits array");
    assert!(
        !b_hits
            .iter()
            .any(|h| h["text"].as_str().unwrap_or("").contains(&nonce)),
        "TENANCY LEAK: tenant B retrieved tenant A's passage. \
         Every Qdrant search must carry .filter(tenant_filter(..)) — invariant 1. Body: {body}"
    );

    // And A still can afterwards — B's search must not have disturbed anything.
    let (status, body) = app.search(&a_key, &passage).await;
    assert_eq!(status, 200);
    assert!(
        body["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["text"].as_str().unwrap_or("").contains(&nonce)),
        "tenant A lost its passage after tenant B searched"
    );

    app.cleanup().await;
}
