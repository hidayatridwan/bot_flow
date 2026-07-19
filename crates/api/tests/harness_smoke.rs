//! Does the harness itself work? Everything else in this suite is worthless if it does not.

mod common;

use axum::{body::Body, http::Request};
use common::TestApp;

/// Proves the fixture boots, the router answers, and — via `rabbitmq: true` — that the fixture is
/// holding the `lapin::Connection` alive. Drop `_amqp` from `TestApp` and only this test notices.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn the_fixture_serves_the_real_router() {
    let app = TestApp::new().await;

    let (status, body) = app
        .request(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(status, 200);
    assert_eq!(
        body["status"], "ok",
        "dependencies not all reachable: {body}"
    );
    assert_eq!(
        body["rabbitmq"], true,
        "rabbitmq is down — the fixture dropped its lapin::Connection, which closes the Channel \
         inside AppState. Nothing else would have told you."
    );

    app.cleanup().await;
}

/// The fake gateway must produce vectors the real client accepts: 1536-dim, indexed, ordered.
/// If this fails, every retrieval test fails for a reason that has nothing to do with tenancy.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn the_fake_gateway_satisfies_the_real_embedding_client() {
    let app = TestApp::new().await;
    let (_tenant, sk) = app.create_tenant().await;

    let (status, body) = app
        .request(
            common::json_request("POST", "/ingest", &sk)
                .body(Body::from(
                    serde_json::json!({"texts": ["hello from the harness"]}).to_string(),
                ))
                .unwrap(),
        )
        .await;

    assert_eq!(status, 200, "ingest failed: {body}");
    assert!(
        app.gateway.counts.embeddings() > 0,
        "the embedding call did not reach the stub — is the base URL seam still wired?"
    );

    app.cleanup().await;
}
