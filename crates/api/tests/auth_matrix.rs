//! Which credential may reach which route.
//!
//! **No unit test can reach this.** `Actor::from_request_parts` resolves a token against the
//! database to decide which principal it is, so the entire gate matrix has only ever been checked
//! by hand with curl — in three separate phases, per the phase-9 design. That is exactly the sort of
//! thing that should stop being manual, and the trap named in CLAUDE.md ("a gate on /ask would 403
//! every deployed widget, and no unit test catches it") is precisely this gap.
//!
//! Three invariants are pinned here:
//! - **15** — a `pk_` is chat-only and origin-bound. `/search` refuses it; a wrong `Origin` refuses it.
//! - **23** — the management gate takes `sk_` *or* `sess_`, never `pk_`.
//! - **27** — the ask routes gate nothing: all three kinds pass, deliberately.

mod common;

use axum::body::Body;
use common::{json_request, TestApp};

const ALLOWED_ORIGIN: &str = "https://allowed.test";

/// One row of the matrix: label, bearer token, method, path, `Origin` header, expected status.
type Case<'a> = (&'a str, &'a str, &'a str, &'a str, Option<&'a str>, u16);

/// What a route needs in its body to get past deserialisation to the gate.
fn body_for(path: &str) -> Body {
    match path {
        "/search" => Body::from(r#"{"query":"anything","limit":3}"#),
        "/ask" => Body::from(r#"{"query":"anything"}"#),
        // Phase 11 contract: a document, not an array of strings.
        "/ingest" => Body::from(r#"{"filename":"anything.md","text":"anything"}"#),
        "/documents/upload-url" => Body::from(r#"{"filename":"anything.txt"}"#),
        _ => Body::empty(),
    }
}

#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn every_credential_reaches_exactly_the_routes_it_should() {
    let app = TestApp::new().await;
    let (tenant_id, sk) = app.create_tenant().await;
    let pk = app
        .mint_key(&tenant_id, "publishable", &[ALLOWED_ORIGIN])
        .await;

    let (_sess_tenant, sess) = app.register_session().await;

    let cases: Vec<Case> = vec![
        // --- Management gate: sk_ and sess_ pass, pk_ is refused (invariants 15, 23) ---
        ("sk_ on /search", &sk, "POST", "/search", None, 200),
        ("sess_ on /search", &sess, "POST", "/search", None, 200),
        // The route that made invariant 15's first sentence false until phase 6. Raw retrieval is
        // not "asking a question". Note this costs no gateway call: require_management() runs
        // before embed_one, so the refusal is free.
        (
            "pk_ on /search",
            &pk,
            "POST",
            "/search",
            Some(ALLOWED_ORIGIN),
            403,
        ),
        ("sk_ on GET /documents", &sk, "GET", "/documents", None, 200),
        (
            "sess_ on GET /documents",
            &sess,
            "GET",
            "/documents",
            None,
            200,
        ),
        (
            "pk_ on GET /documents",
            &pk,
            "GET",
            "/documents",
            Some(ALLOWED_ORIGIN),
            403,
        ),
        (
            "sess_ on upload-url",
            &sess,
            "POST",
            "/documents/upload-url",
            None,
            201,
        ),
        // --- Ask routes: no gate at all, on purpose (invariant 27) ---
        // A gate here would 403 every deployed widget — the one client these routes exist for.
        (
            "pk_ on /ask",
            &pk,
            "POST",
            "/ask",
            Some(ALLOWED_ORIGIN),
            200,
        ),
        ("sk_ on /ask", &sk, "POST", "/ask", None, 200),
        ("sess_ on /ask", &sess, "POST", "/ask", None, 200),
        // --- The pk_ origin allow-list: containment, enforced server-side (invariant 15) ---
        (
            "pk_ from a foreign origin",
            &pk,
            "POST",
            "/ask",
            Some("https://evil.test"),
            403,
        ),
        // No Origin at all is refused too: the check is `matches!(origin, Some(o) if ..)`, so an
        // absent header can never match. A `pk_` is a browser credential; a caller without an
        // Origin is not the browser it was minted for.
        ("pk_ with no Origin", &pk, "POST", "/ask", None, 403),
        // --- Secret-only routes: not being extended, so no session (require_secret) ---
        // 202: indexing is asynchronous as of phase 11 — the worker does it.
        ("sk_ on /ingest", &sk, "POST", "/ingest", None, 202),
        // 401, not 403: /ingest takes `AuthTenant`, which resolves bearer tokens against `api_keys`
        // only. A `sess_` is not in that table, so it misses and is simply an unknown key. The two
        // tables are disjoint by prefix (invariant 17) — this is that dispatch observed from outside.
        ("sess_ on /ingest", &sess, "POST", "/ingest", None, 401),
        // --- Unknown credentials: 401, never 403 ---
        (
            "garbage token on /ask",
            "sk_totally-made-up",
            "POST",
            "/ask",
            None,
            401,
        ),
        (
            "garbage token on /documents",
            "nonsense",
            "GET",
            "/documents",
            None,
            401,
        ),
    ];

    let mut failures = Vec::new();
    for (label, token, method, path, origin, expected) in cases {
        let mut req = json_request(method, path, token);
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        let (status, body) = app.request(req.body(body_for(path)).unwrap()).await;

        if status.as_u16() != expected {
            failures.push(format!(
                "  {label}: expected {expected}, got {status} — {body}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "auth matrix violations:\n{}\n\
         A 403 where 200 was expected on an ask route means a gate was added to a route a `pk_` \
         must reach (invariant 27). A 200 where 403 was expected means containment was widened.",
        failures.join("\n")
    );

    app.cleanup().await;
}

/// 401 and 403 answer different questions, and conflating them makes the endpoint an oracle.
///
/// 401 = this token resolved to nothing. 403 = this principal resolved, and is refused. A `pk_`
/// getting 401 on `/search` would tell an attacker their stolen key is invalid, when in fact it is
/// perfectly valid and merely out of scope.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn a_refused_principal_is_distinguishable_from_an_unknown_one() {
    let app = TestApp::new().await;
    let (tenant_id, _sk) = app.create_tenant().await;
    let pk = app
        .mint_key(&tenant_id, "publishable", &[ALLOWED_ORIGIN])
        .await;

    let (refused, _) = app
        .request(
            json_request("POST", "/search", &pk)
                .header("origin", ALLOWED_ORIGIN)
                .body(body_for("/search"))
                .unwrap(),
        )
        .await;
    let (unknown, _) = app
        .request(
            json_request("POST", "/search", "sk_not-a-real-key")
                .body(body_for("/search"))
                .unwrap(),
        )
        .await;

    assert_eq!(refused, 403, "a valid pk_ out of scope must be 403");
    assert_eq!(unknown, 401, "an unresolvable token must be 401");

    app.cleanup().await;
}
