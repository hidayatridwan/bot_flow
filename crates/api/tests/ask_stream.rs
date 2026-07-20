//! `/ask/stream` — the frame contract, and the turn it leaves behind.
//!
//! **This route had no test at all**, which CLAUDE.md listed as a known coverage gap. It acquired
//! one when its token loop was restructured to carry a wall-clock deadline (blocker 5): a change to
//! the loop that emits every frame a widget consumes deserved something more than "it compiles".
//!
//! What these cover is the *shape* of a stream and its side effect on history. What they do not
//! cover is the deadline firing, and that is stated plainly rather than faked: `STREAM_DEADLINE` is
//! five minutes, so a test that waited for it would be a five-minute test, and one that faked the
//! clock would be asserting against a stub rather than the code. The gap is named in the phase doc.

mod common;

use axum::body::Body;
use common::{json_request, TestApp};

/// Split an SSE body into `(event, data)` pairs.
///
/// Data lines are joined with `\n`, not `''`. axum re-prefixes every newline inside a value with a
/// fresh `data: `, so one token can arrive as several lines — joining without the newline silently
/// collapses a numbered list into one line, and the answer still looks plausible. The widget's
/// decoder makes the same choice, and for the same reason.
fn frames(body: &str) -> Vec<(String, String)> {
    body.split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .map(|block| {
            let mut event = String::new();
            let mut data: Vec<&str> = Vec::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event: ") {
                    event = v.to_string();
                } else if let Some(v) = line.strip_prefix("data: ") {
                    data.push(v);
                } else if line == "data:" {
                    data.push("");
                }
            }
            (event, data.join("\n"))
        })
        .collect()
}

/// The happy path, frame by frame, plus the turn it must leave in history.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn a_streamed_answer_emits_the_contract_and_persists_the_turn() {
    let app = TestApp::new().await;
    let (tenant, sk) = app.create_tenant().await;
    app.plant_chunk(&tenant, "Refunds are accepted within 30 days of purchase.")
        .await;

    let body = app
        .sse(
            &sk,
            r#"{"query":"Refunds are accepted within 30 days of purchase.","limit":3}"#,
        )
        .await;
    let frames = frames(&body);
    let names: Vec<&str> = frames.iter().map(|(e, _)| e.as_str()).collect();

    // Order is the contract: a client cannot attribute tokens to a conversation it has not been
    // told about, nor render a citation for sources that arrive after the prose.
    assert_eq!(
        names.first(),
        Some(&"conversation"),
        "the conversation id must come first: {names:?}"
    );
    assert_eq!(
        names.get(1),
        Some(&"sources"),
        "sources must precede the tokens: {names:?}"
    );
    assert_eq!(
        names.last(),
        Some(&"done"),
        "a completed answer must end with `done`: {names:?}"
    );
    assert!(
        !names.contains(&"error"),
        "a healthy answer emitted an error frame: {names:?}"
    );

    // `done` carries no `data:` line at all — `Event::data("")` writes nothing. That is exactly why
    // the widget cannot use a browser `EventSource`, whose dispatch step drops a data-less event:
    // it would never fire `done`, silently, and every finished answer would look truncated.
    let (_, done_data) = frames.last().unwrap();
    assert_eq!(done_data, "", "`done` must carry no data: {done_data:?}");

    let answer: String = frames
        .iter()
        .filter(|(e, _)| e == "token")
        .map(|(_, d)| d.as_str())
        .collect();
    assert!(
        answer.contains(common::FAKE_ANSWER),
        "the streamed tokens did not reassemble into the gateway's answer: {answer:?}"
    );

    // Invariant 7: the turn exists precisely because an answer does. Two rows — the question and
    // the answer — written in one transaction.
    let conversation_id = &frames[0].1;
    let turns = app
        .count_as_tenant(
            &tenant,
            // `#>> '{}'` unwraps the JSON string to text — `count_as_tenant` binds jsonb, which
            // Postgres will not cast straight to uuid.
            "SELECT count(*) FROM messages WHERE conversation_id::text = ($1 #>> '{}')",
            serde_json::json!(conversation_id),
        )
        .await;
    assert_eq!(
        turns, 2,
        "a completed stream must leave the question and its answer in history"
    );
}

/// A refusal is an answer (invariant 4), and it must reach the client as one.
///
/// The LLM is never called here — nothing clears the relevance floor — so this asserts the branch
/// that decides *not* to spend money, which is the branch with no other observable.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn a_refusal_streams_as_a_normal_answer_not_an_error() {
    let app = TestApp::new().await;
    let (tenant, sk) = app.create_tenant().await;
    app.plant_chunk(&tenant, "The office is on the fourth floor.")
        .await;

    let before = app.gateway.counts.completions();
    let body = app
        .sse(
            &sk,
            r#"{"query":"utterly unrelated question about penguins"}"#,
        )
        .await;
    let frames = frames(&body);
    let names: Vec<&str> = frames.iter().map(|(e, _)| e.as_str()).collect();

    assert!(
        !names.contains(&"error"),
        "a refusal is a successful answer, not a failure: {names:?}"
    );
    assert_eq!(
        names.last(),
        Some(&"done"),
        "a refusal still ends: {names:?}"
    );
    assert_eq!(
        app.gateway.counts.completions(),
        before,
        "nothing cleared the relevance floor, so the LLM must not have been called at all"
    );

    // Persisted too: history is a faithful record of what the user was told, including "I don't
    // know". Otherwise the next rewrite reasons over a question that appears unanswered.
    let conversation_id = &frames[0].1;
    let turns = app
        .count_as_tenant(
            &tenant,
            // `#>> '{}'` unwraps the JSON string to text — `count_as_tenant` binds jsonb, which
            // Postgres will not cast straight to uuid.
            "SELECT count(*) FROM messages WHERE conversation_id::text = ($1 #>> '{}')",
            serde_json::json!(conversation_id),
        )
        .await;
    assert_eq!(turns, 2, "a refusal must still be recorded as a turn");
}

/// The route a `pk_` exists to reach (invariant 27), asserted on the *streaming* twin.
///
/// `auth_matrix.rs` pins this for `/ask`; the pair must not diverge on who may call them, and a
/// gate added here would 403 every deployed widget while every unit test stayed green.
#[tokio::test]
#[ignore = "needs docker compose services + the bot_flow_test database"]
async fn a_publishable_key_may_stream_from_an_allow_listed_origin() {
    let app = TestApp::new().await;
    let (tenant, _sk) = app.create_tenant().await;
    let pk = app
        .mint_key(&tenant, "publishable", &["https://acme.example"])
        .await;
    app.plant_chunk(&tenant, "Support is available on weekdays.")
        .await;

    let (status, _) = app
        .request(
            json_request("POST", "/ask/stream", &pk)
                .header("origin", "https://acme.example")
                .body(Body::from(
                    r#"{"query":"Support is available on weekdays."}"#,
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(
        status, 200,
        "a pk_ from an allow-listed origin must reach /ask/stream — it is the one route the \
         weakest credential exists to call"
    );
}
