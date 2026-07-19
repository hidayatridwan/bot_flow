//! `/metrics` — that it is guarded, and that its numbers actually move.
//!
//! **A counter that never changes is decoration**, and decoration on a monitoring endpoint is worse
//! than nothing: it is a number that gets believed. This is the same argument the retrieval bench's
//! sabotage table makes, applied to the instrument rather than the metric.
//!
//! The gauges are the more interesting half. `documents` is RLS-forced and the API connects as
//! `app_user`, so a plain aggregate would match zero rows and *report success* — every gauge
//! permanently 0, dashboard permanently green. That is why they go through `SECURITY DEFINER`
//! functions (migration 0014), and why the assertion below is that a seeded document actually
//! *appears*.

mod common;

use axum::{body::Body, http::Request};
use common::{json_request, TestApp};

async fn scrape(app: &TestApp, token: &str) -> (u16, String) {
    let res = app
        .request(
            Request::builder()
                .uri("/metrics")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    // The body is text, not JSON, so `request`'s JSON parse yields Null — scrape it raw instead.
    (res.0.as_u16(), app.text("/metrics", token).await)
}

#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn metrics_requires_its_own_token() {
    let app = TestApp::new().await;

    let (status, _) = scrape(&app, "totally-wrong").await;
    assert_eq!(status, 401, "a wrong token must not scrape");

    // The admin key must NOT work here. A scrape config is a widely-readable artifact, and the
    // admin key can erase a tenant (phase 12) — that trade is the reason METRICS_TOKEN exists.
    let (status, _) = scrape(&app, &app.admin_key).await;
    assert_eq!(
        status, 401,
        "ADMIN_API_KEY must not scrape /metrics: it can delete tenants, and a scrape config is not \
         a place to put that"
    );

    let (status, body) = scrape(&app, &app.metrics_token).await;
    assert_eq!(status, 200);
    assert!(body.contains("botflow_ask_total"));
}

/// The gauge half — and the RLS trap it exists to dodge.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn document_gauges_see_across_tenants_despite_rls() {
    let app = TestApp::new().await;
    let (_tenant, sk) = app.create_tenant().await;

    let (status, _) = app
        .request(
            json_request("POST", "/documents/upload-url", &sk)
                .body(Body::from(r#"{"filename":"metrics.md"}"#))
                .unwrap(),
        )
        .await;
    assert_eq!(status, 201);

    let body = app.text("/metrics", &app.metrics_token).await;
    assert!(
        body.contains(r#"botflow_documents{status="uploading"}"#),
        "no document gauge appeared. If every gauge reads 0, the aggregate is being scoped by RLS \
         to a tenant that is never set — it returns zero rows and reports success, and the \
         dashboard is permanently green. That is what migration 0014's SECURITY DEFINER functions \
         are for. Body:\n{body}"
    );
    assert!(body.contains("botflow_tenants "));

    app.cleanup().await;
}

/// The canary moves, and moves only when it should.
///
/// A refusal is a `200` with an answer, so it is invisible to every status-code metric. If this
/// counter did not move, the one production signal that retrieval has quietly stopped working
/// would be a constant.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn the_refusal_counter_moves_only_on_a_refusal() {
    let app = TestApp::new().await;
    let (tenant, sk) = app.create_tenant().await;

    let before = app.metric("botflow_ask_refused_total").await;
    let asks_before = app.metric("botflow_ask_total").await;

    // Nothing indexed for this tenant, so nothing can clear the floor: a refusal by construction.
    let (status, _) = app
        .request(
            json_request("POST", "/ask", &sk)
                .body(Body::from(r#"{"query":"anything at all"}"#))
                .unwrap(),
        )
        .await;
    assert_eq!(
        status, 200,
        "a refusal is a 200 — that is the whole problem"
    );

    assert_eq!(
        app.metric("botflow_ask_refused_total").await,
        before + 1,
        "the refusal was not counted"
    );
    assert_eq!(app.metric("botflow_ask_total").await, asks_before + 1);

    // Now one that finds something: total moves, refusals do not.
    app.plant_chunk(&tenant, "the answer is fourty two").await;
    let refused_now = app.metric("botflow_ask_refused_total").await;
    let (status, _) = app
        .request(
            json_request("POST", "/ask", &sk)
                .body(Body::from(r#"{"query":"the answer is fourty two"}"#))
                .unwrap(),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        app.metric("botflow_ask_refused_total").await,
        refused_now,
        "an ANSWERED question incremented the refusal counter — the ratio is now meaningless, and \
         it is the only signal that retrieval has degraded"
    );

    app.cleanup().await;
}
