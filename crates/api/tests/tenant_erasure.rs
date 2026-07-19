//! Erasing a tenant, and proving it happened.
//!
//! Phase 11 made a *document* erasable. This is the other half of the same obligation: a processor
//! asked to *"delete everything you hold about us"* had, until now, no way to do it —
//! `DELETE FROM tenants` cascaded in Postgres and left every vector and every object standing.
//!
//! Two properties are asserted separately because they fail separately: that the data is gone, and
//! that the **record of erasing it survives the erasure**. The second is the one a naive
//! implementation gets wrong, by giving the audit table a foreign key like every other table has.

mod common;

use axum::body::Body;
use common::{json_request, TestApp};
use sqlx::Row;

/// The whole claim: a tenant's data goes, across all three stores.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn erasing_a_tenant_removes_its_vectors_rows_and_access() {
    let app = TestApp::new().await;
    let (doomed, doomed_key) = app.create_tenant().await;
    let (survivor, survivor_key) = app.create_tenant().await;

    // Both tenants have data. The survivor is the control: an erasure that took everything would
    // pass every assertion about the doomed tenant.
    let nonce = format!("nonce-{}", uuid::Uuid::new_v4().simple());
    app.plant_chunk(&doomed, &format!("{nonce} doomed tenant passage"))
        .await;
    app.plant_chunk(&survivor, &format!("{nonce} survivor passage"))
        .await;

    // A document row and a key, so the cascade has something to take.
    let (status, _) = app
        .request(
            json_request("POST", "/documents/upload-url", &doomed_key)
                .body(Body::from(r#"{"filename":"doomed.md"}"#))
                .unwrap(),
        )
        .await;
    assert_eq!(status, 201);

    // Erase.
    let (status, body) = app
        .request(
            json_request(
                "DELETE",
                &format!("/admin/tenants/{doomed}"),
                &app.admin_key,
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(status, 200, "tenant erasure failed: {body}");
    assert!(
        body["vectors_deleted"].as_u64().unwrap_or(0) >= 1,
        "erasure must report what it removed — a caller acting on a request needs evidence: {body}"
    );

    // 1. The rows are gone.
    let remaining: i64 = sqlx::query("SELECT count(*) FROM tenants WHERE id = $1")
        .bind(&doomed)
        .fetch_one(&app.db)
        .await
        .unwrap()
        .get(0);
    assert_eq!(remaining, 0, "the tenant row survived its own erasure");

    // 2. Its credential no longer authenticates anything.
    let (status, _) = app
        .request(
            json_request("GET", "/documents", &doomed_key)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        status, 401,
        "an erased tenant's key must stop working — access is revoked before anything is deleted"
    );

    // 3. THE CONTROL: the other tenant is untouched. A `delete_points` missing its tenant filter
    //    would erase the whole collection and every assertion above would still pass.
    let (status, body) = app
        .search(&survivor_key, &format!("{nonce} survivor passage"))
        .await;
    assert_eq!(status, 200);
    assert!(
        body["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["text"].as_str().unwrap_or("").contains(&nonce)),
        "ERASURE LEAK: erasing one tenant removed another's vectors. Body: {body}"
    );

    app.cleanup().await;
}

/// The audit record must **outlive** the thing it records.
///
/// Every other tenant-scoped table has `references tenants(id) on delete cascade`, which is right
/// for data and catastrophic for an audit log: it would be destroyed by the erasure it documents,
/// and the destruction would look like diligence. `erasures` has no foreign key, deliberately.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn the_erasure_record_survives_the_erasure() {
    let app = TestApp::new().await;
    let (doomed, _key) = app.create_tenant().await;

    let (status, body) = app
        .request(
            json_request(
                "DELETE",
                &format!("/admin/tenants/{doomed}"),
                &app.admin_key,
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(status, 200, "{body}");

    let row = sqlx::query(
        "SELECT scope, actor, completed_at IS NOT NULL AS done FROM erasures WHERE tenant_id = $1",
    )
    .bind(&doomed)
    .fetch_optional(&app.db)
    .await
    .unwrap();

    let row = row.expect(
        "the erasure record was destroyed by the erasure it documents — `erasures` must NOT have a \
         foreign key to `tenants`, or the audit trail cascades away exactly when it matters",
    );
    assert_eq!(row.get::<String, _>("scope"), "tenant");
    assert_eq!(row.get::<String, _>("actor"), "admin");
    assert!(
        row.get::<bool, _>("done"),
        "an erasure that finished must say so — a row with completed_at null is one that did not"
    );
}

/// A typo must not read as a completed erasure.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn erasing_an_unknown_tenant_is_a_404() {
    let app = TestApp::new().await;
    let (status, _) = app
        .request(
            json_request(
                "DELETE",
                "/admin/tenants/no-such-tenant-here",
                &app.admin_key,
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(status, 404);
}

/// Only the admin key may end a tenant.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn a_tenant_cannot_erase_itself_or_anyone_else() {
    let app = TestApp::new().await;
    let (victim, _) = app.create_tenant().await;
    let (_other, other_key) = app.create_tenant().await;

    for (label, token) in [
        ("its own sk_", &other_key),
        ("garbage", &"sk_nope".to_string()),
    ] {
        let (status, _) = app
            .request(
                json_request("DELETE", &format!("/admin/tenants/{victim}"), token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            status, 401,
            "{label} must not reach an admin route — /admin/* is guarded by a deployment secret, \
             not a database row"
        );
    }

    // And the victim is still there.
    let remaining: i64 = sqlx::query("SELECT count(*) FROM tenants WHERE id = $1")
        .bind(&victim)
        .fetch_one(&app.db)
        .await
        .unwrap()
        .get(0);
    assert_eq!(remaining, 1);

    app.cleanup().await;
}
