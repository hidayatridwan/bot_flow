//! `GET /documents` is paginated, and its cursor is stable under the writes a poll actually sees.
//!
//! Blocker 5's first item: the listing returned the tenant's entire table, fully materialised, on
//! every call — and the dashboard polls it. The fix is a keyset cursor, and the reason these tests
//! exist rather than a single "it returns 50 rows" assertion is that **every way a cursor goes wrong
//! is silent**. A page that skips a row still returns a plausible list. A page that repeats one
//! still renders. Nothing 500s, nothing logs, and the tenant simply cannot find a document.
//!
//! The interesting case is the one this table actually produces: `created_at` defaults to `now()`,
//! which is `transaction_timestamp()`, so **rows written in one transaction share a byte-identical
//! timestamp**. A cursor on `created_at` alone loses exactly the rows sitting on a page boundary.

mod common;

use axum::body::Body;
use common::{json_request, TestApp};
use std::collections::HashSet;

/// Ingest `n` documents and return their ids. Sequential on purpose — each `/ingest` is its own
/// transaction, so this produces *some* distinct timestamps and, at this speed, some collisions too.
async fn seed(app: &TestApp, sk: &str, n: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for i in 0..n {
        let body = serde_json::json!({
            "filename": format!("doc-{i:03}.md"),
            "text": format!("Document number {i}."),
        });
        let (status, body) = app
            .request(
                json_request("POST", "/ingest", sk)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 202, "seeding failed at {i}: {body}");
        ids.push(body["document_id"].as_str().unwrap().to_string());
    }
    ids
}

/// Walk every page, following `next_cursor` until it is null. Returns the ids in the order seen.
async fn drain(app: &TestApp, sk: &str, limit: usize) -> Vec<String> {
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    // A loop over a cursor the server controls needs its own bound: a cursor bug that fails to
    // advance would otherwise hang the suite rather than fail it.
    for _ in 0..50 {
        let path = match &cursor {
            Some(c) => format!("/documents?limit={limit}&before={}", urlencode(c)),
            None => format!("/documents?limit={limit}"),
        };
        let (status, body) = app
            .request(json_request("GET", &path, sk).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, 200, "page request failed: {body}");

        let page = body["documents"].as_array().unwrap().clone();
        assert!(
            page.len() <= limit,
            "a page returned {} rows for limit={limit}",
            page.len()
        );
        seen.extend(
            page.iter()
                .filter_map(|d| d["id"].as_str().map(String::from)),
        );

        match body["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => return seen,
        }
    }
    panic!("pagination did not terminate — the cursor is not advancing");
}

/// Minimal percent-encoding for the cursor. It carries `+` (the UTC offset), which decodes to a
/// space and would corrupt the timestamp — the exact trap the `T` separator already avoids for the
/// date/time split.
fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '+' => "%2B".to_string(),
            ':' => "%3A".to_string(),
            c => c.to_string(),
        })
        .collect()
}

/// The core property: paging through the table yields every row exactly once.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn paging_yields_every_document_exactly_once() {
    let app = TestApp::new().await;
    let (_tenant, sk) = app.create_tenant().await;

    let created = seed(&app, &sk, 12).await;

    // A limit that does not divide the total, so the last page is partial and at least one boundary
    // falls mid-run. Both are where an off-by-one hides.
    let seen = drain(&app, &sk, 5).await;

    assert_eq!(
        seen.len(),
        created.len(),
        "paging returned {} ids for {} documents — a cursor that skips or repeats is the only way \
         this differs, and neither shows up as an error",
        seen.len(),
        created.len()
    );
    assert_eq!(
        seen.iter().collect::<HashSet<_>>(),
        created.iter().collect::<HashSet<_>>(),
        "the set of paged ids differs from the set created"
    );

    let unique: HashSet<&String> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "a document was returned twice");
}

/// The tie case, which is not hypothetical: `now()` is `transaction_timestamp()`.
///
/// These rows are written in **one** transaction, so all of them carry the identical `created_at`.
/// Ordering by that column alone is non-deterministic between calls, and a cursor built from it
/// cannot say where a page ended. This is the test that fails if the `id` tiebreaker is dropped from
/// either the `ORDER BY` or the `WHERE`.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn a_page_boundary_inside_identical_timestamps_loses_nothing() {
    let app = TestApp::new().await;
    let (tenant, sk) = app.create_tenant().await;

    // Straight to the database: the point is that every row shares one transaction timestamp, which
    // no sequence of HTTP calls can reliably produce.
    let mut tx = app.tenant_tx(&tenant).await;
    for i in 0..9 {
        sqlx::query(
            "INSERT INTO documents (id, tenant_id, filename, object_key, status)
             VALUES (gen_random_uuid(), $1, $2, $3, 'ready')",
        )
        .bind(&tenant)
        .bind(format!("tied-{i}.md"))
        .bind(format!("tenants/{tenant}/documents/tied-{i}/original.md"))
        .execute(&mut *tx)
        .await
        .expect("failed to seed tied rows");
    }
    tx.commit().await.expect("failed to commit tied rows");

    // Confirm the premise rather than assuming it — if `created_at` ever stopped being
    // transaction-scoped, this test would silently stop testing anything.
    let distinct: i64 = {
        let mut tx = app.tenant_tx(&tenant).await;
        let n = sqlx::query_scalar("SELECT count(DISTINCT created_at) FROM documents")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        n
    };
    assert_eq!(
        distinct, 1,
        "premise broken: these rows were supposed to share one transaction timestamp"
    );

    // Limit 4 over 9 identical timestamps: both boundaries fall strictly inside the tie.
    let seen = drain(&app, &sk, 4).await;
    let unique: HashSet<&String> = seen.iter().collect();

    assert_eq!(
        seen.len(),
        9,
        "paging across identical timestamps returned {} of 9 rows — the `id` tiebreaker is what \
         makes the boundary well-defined",
        seen.len()
    );
    assert_eq!(unique.len(), 9, "a row inside the tie was returned twice");
}

/// An un-updated client sends no parameters and must still be bounded — that is what actually
/// closes the unbounded read, rather than merely offering a way to avoid it.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn the_default_page_is_bounded_and_the_contract_still_holds() {
    let app = TestApp::new().await;
    let (_tenant, sk) = app.create_tenant().await;
    seed(&app, &sk, 3).await;

    let (status, body) = app
        .request(
            json_request("GET", "/documents", &sk)
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(status, 200);
    // The shape every existing client already reads.
    assert!(
        body["documents"].is_array(),
        "documents[] must survive: {body}"
    );
    assert_eq!(body["documents"].as_array().unwrap().len(), 3);
    // Well under one page, so this is the last page and says so.
    assert!(
        body["next_cursor"].is_null(),
        "a complete listing must report no next page: {body}"
    );
    assert_eq!(
        body["limit"].as_u64(),
        Some(50),
        "the default page size is the bound an un-updated client receives"
    );
}

/// A cursor is our token. Anything else is the caller's mistake, and must not read as ours.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn a_bad_cursor_or_limit_is_the_callers_error_not_a_500() {
    let app = TestApp::new().await;
    let (_tenant, sk) = app.create_tenant().await;

    // Each of these would otherwise reach a `::timestamptz` cast, where the database raises an
    // error that `?` turns into a 500 — an internal error for plainly malformed input.
    for bad in [
        "/documents?before=nonsense",
        "/documents?before=9999-99-99T99%3A99%3A99%2B00~5d2810fc-4117-4b34-b4a4-37009bffee40",
        "/documents?before=2026-07-20T04%3A09%3A35%2B00~not-a-uuid",
    ] {
        let (status, body) = app
            .request(json_request("GET", bad, &sk).body(Body::empty()).unwrap())
            .await;
        assert_eq!(status, 422, "expected 422 for {bad}: {body}");
    }

    // The ceiling: without it, `?limit=100000` reinstates exactly the read this closes.
    let (status, _) = app
        .request(
            json_request("GET", "/documents?limit=100000", &sk)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, 422, "an unbounded limit must be refused");

    // A non-numeric limit never reaches our validator — axum rejects the query string itself, and
    // a malformed request is a 400 rather than a 422 (the repo's own split).
    let (status, _) = app
        .request(
            json_request("GET", "/documents?limit=lots", &sk)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, 400, "a malformed query string is a 400, not a 422");
}
