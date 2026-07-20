//! Password reset and change — the flow that must work for someone who has lost their credential.
//!
//! Blocker 6: before this, a forgotten password was the end of the account. There was no endpoint,
//! no token, no mail transport, and every lockout was a manual database edit by an operator who
//! could not verify the requester owned the address.
//!
//! These tests are about the *security properties*, not the happy path, because the happy path is
//! the part anyone would notice was broken. Every property below fails **silently**: a reset that
//! leaves old sessions alive still logs you in, a replayable token still works the first time, and
//! an endpoint that 404s an unknown address still resets real passwords perfectly.
//!
//! **What they do not cover: delivery.** The harness points `SMTP_URL` at a dead port, so the
//! spawned send always fails and is logged. That is deliberate — a mail assertion here would be an
//! assertion about a stub. The real path (compose up Mailpit → request a reset → open the link from
//! the actual email → redeem it) was run by hand and is recorded in the phase doc.

mod common;

use axum::body::Body;
use common::{json_request, TestApp};

fn body(v: serde_json::Value) -> Body {
    Body::from(v.to_string())
}

/// Register a tenant and return `(tenant_id, email, session_token)`.
async fn account(app: &TestApp) -> (String, String, String) {
    let (tenant, session) = app.register_session().await;
    let email = TestApp::account_email(&tenant);
    (tenant, email, session)
}

/// Redeeming a link must end every session the account had.
///
/// **The assertion that matters most.** Someone resetting a password may be recovering *from* a
/// compromise; a reset that leaves the attacker's session alive is theatre. And it fails completely
/// silently — the user sees a success page and a working new password either way.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn a_reset_revokes_every_existing_session() {
    let app = TestApp::new().await;
    let (_tenant, email, first) = account(&app).await;

    // A second session, as if the user were signed in on another device.
    let second = app.login(&email, TestApp::ACCOUNT_PASSWORD).await;
    for (name, s) in [("first", &first), ("second", &second)] {
        let (status, _) = app
            .request(
                json_request("GET", "/auth/me", s)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            status, 200,
            "the {name} session should be live before the reset"
        );
    }

    let token = app.issue_reset_token(&email, false).await;
    let (status, b) = app
        .request(
            json_request("POST", "/auth/password/reset", "")
                .body(body(
                    serde_json::json!({ "token": token, "password": "a-brand-new-password" }),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(status, 204, "reset failed: {b}");

    for (name, s) in [("first", &first), ("second", &second)] {
        let (status, _) = app
            .request(
                json_request("GET", "/auth/me", s)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            status, 401,
            "the {name} session survived a password reset — whoever stole it still has the account, \
             which is precisely what the person resetting was trying to stop"
        );
    }
}

/// A reset link works exactly once, and the first redemption is the one that counts.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn a_reset_token_cannot_be_replayed() {
    let app = TestApp::new().await;
    let (_tenant, email, _s) = account(&app).await;
    let token = app.issue_reset_token(&email, false).await;

    let (first, _) = app
        .request(
            json_request("POST", "/auth/password/reset", "")
                .body(body(
                    serde_json::json!({ "token": token, "password": "first-new-password" }),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(first, 204);

    let (replay, _) = app
        .request(
            json_request("POST", "/auth/password/reset", "")
                .body(body(
                    serde_json::json!({ "token": token, "password": "second-new-password" }),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(
        replay, 400,
        "a spent link stayed live — an old email in an archive would remain a key to the account"
    );

    // The first redemption is what took effect, not the replay's password.
    let (status, _) = app
        .request(
            json_request("POST", "/auth/login", "")
                .body(body(
                    serde_json::json!({ "email": email, "password": "first-new-password" }),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(status, 200, "the first redemption should have won");
}

/// Redeeming one link burns the account's other outstanding links.
///
/// Ask three times, use one, and the two older emails must be dead — otherwise they stay live for
/// the rest of the hour, after the account has already been secured.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn redeeming_one_link_burns_the_others() {
    let app = TestApp::new().await;
    let (_tenant, email, _s) = account(&app).await;

    let older = app.issue_reset_token(&email, false).await;
    let newer = app.issue_reset_token(&email, false).await;

    let (status, _) = app
        .request(
            json_request("POST", "/auth/password/reset", "")
                .body(body(
                    serde_json::json!({ "token": newer, "password": "chosen-via-newer-link" }),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(status, 204);

    let (status, _) = app
        .request(
            json_request("POST", "/auth/password/reset", "")
                .body(body(
                    serde_json::json!({ "token": older, "password": "chosen-via-older-link" }),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(
        status, 400,
        "an earlier link was still redeemable after the account was recovered"
    );
}

/// The endpoint must not reveal which addresses are registered — invariant 18's non-oracle rule,
/// arriving at a third public endpoint.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn forgot_password_is_not_an_existence_oracle() {
    let app = TestApp::new().await;
    let (_tenant, email, _s) = account(&app).await;

    let mut seen = Vec::new();
    for address in [
        email.as_str(),                     // registered
        "definitely-not-here@nowhere.test", // unregistered
        "not-an-email-at-all",              // garbage
        "",                                 // empty
    ] {
        let (status, json) = app
            .request(
                json_request("POST", "/auth/password/forgot", "")
                    .body(body(serde_json::json!({ "email": address })))
                    .unwrap(),
            )
            .await;
        seen.push((address, status, json.to_string()));
    }

    let (_, want_status, want_body) = &seen[0];
    for (address, status, json) in &seen {
        assert_eq!(
            (status, json),
            (want_status, want_body),
            "the response for {address:?} differed from the response for a registered address — \
             status or body is an oracle for which emails exist, which is exactly what \
             /auth/login refuses to be"
        );
    }
}

/// An expired link is refused, and refused *identically* to a forged one.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn an_expired_link_is_refused_like_a_forged_one() {
    let app = TestApp::new().await;
    let (_tenant, email, _s) = account(&app).await;

    let expired = app.issue_reset_token(&email, true).await;
    let (expired_status, expired_body) = app
        .request(
            json_request("POST", "/auth/password/reset", "")
                .body(body(
                    serde_json::json!({ "token": expired, "password": "does-not-matter-here" }),
                ))
                .unwrap(),
        )
        .await;

    let (forged_status, forged_body) = app
        .request(
            json_request("POST", "/auth/password/reset", "")
                .body(body(serde_json::json!({
                    "token": "rst_0000000000000000000000000000000000000000000000000000000000000000",
                    "password": "does-not-matter-here"
                })))
                .unwrap(),
        )
        .await;

    assert_eq!(expired_status, 400);
    assert_eq!(
        (expired_status, expired_body.to_string()),
        (forged_status, forged_body.to_string()),
        "an expired link and a forged one must be indistinguishable — telling them apart says \
         'that token was real once', which is a fact about the account"
    );
}

/// Changing a password requires the current one, even though the caller already holds a session.
///
/// A session is a bearer token; one that has been stolen must not be enough to take the account.
/// And the refusal must be **403, not 401** — the web BFF clears its session cookie on a 401
/// (invariant 21), so a 401 here would sign a user out for mistyping their own password.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn changing_a_password_needs_the_current_one_and_refuses_with_403() {
    let app = TestApp::new().await;
    let (_tenant, email, session) = account(&app).await;
    let other_device = app.login(&email, TestApp::ACCOUNT_PASSWORD).await;

    let (status, _) = app
        .request(
            json_request("POST", "/auth/password", &session)
                .body(body(serde_json::json!({
                    "current_password": "not-the-right-one",
                    "new_password": "attackers-choice-here"
                })))
                .unwrap(),
        )
        .await;
    assert_eq!(
        status, 403,
        "a wrong current password must be 403: a 401 tells the BFF the session died, and it would \
         log the user out for a typo"
    );

    let (status, _) = app
        .request(
            json_request("POST", "/auth/password", &session)
                .body(body(serde_json::json!({
                    "current_password": TestApp::ACCOUNT_PASSWORD,
                    "new_password": "a-legitimate-new-one"
                })))
                .unwrap(),
        )
        .await;
    assert_eq!(status, 204);

    // This session survives: the user is right here, and signing them out of the tab they just used
    // would punish the person doing the right thing.
    let (status, _) = app
        .request(
            json_request("GET", "/auth/me", &session)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        status, 200,
        "the session that changed the password must survive"
    );

    // Every other one does not.
    let (status, _) = app
        .request(
            json_request("GET", "/auth/me", &other_device)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        status, 401,
        "another device stayed signed in after a password change — if that device was the reason \
         for the change, nothing was achieved"
    );
}
