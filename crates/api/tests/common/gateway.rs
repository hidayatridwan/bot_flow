//! A fake OpenAI-compatible gateway, reached through the existing base-URL seam.
//!
//! **These tests must never make a billed call.** `/search` embeds its query and `/ask` calls the
//! LLM, so a suite that pointed at a real gateway would spend money per test per CI run, be
//! nondeterministic, need CI secrets, and fail whenever the network did. `EmbeddingClient` and
//! `LlmClient` are concrete structs with no trait — but both are built from a base URL that comes
//! from `Config`, so a stub needs **zero production code change**. Introducing an `Arc<dyn Embedder>`
//! purely to test would churn `common` (shared with the worker) and add dynamic dispatch on the hot
//! path; that is the "don't widen for tests" rule in its dependency-injection form.
//!
//! **The honest boundary:** this verifies *our* logic, not our contract with the real gateway. If a
//! provider changed its response shape tomorrow, nothing here would go red.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use axum::{extract::State, routing::post, Json, Router};
use common::embedding::EMBEDDING_DIM;
use serde_json::{json, Value};

/// How many times each endpoint was called. Lets a test assert invariant 4 *directly* — that a
/// question clearing nothing returns the canned refusal **and never reaches the LLM at all**,
/// rather than inferring it from the response text.
#[derive(Clone, Default)]
pub struct GatewayCounts {
    pub embeddings: Arc<AtomicUsize>,
    pub completions: Arc<AtomicUsize>,
}

impl GatewayCounts {
    pub fn embeddings(&self) -> usize {
        self.embeddings.load(Ordering::SeqCst)
    }
    pub fn completions(&self) -> usize {
        self.completions.load(Ordering::SeqCst)
    }
    pub fn reset(&self) {
        self.embeddings.store(0, Ordering::SeqCst);
        self.completions.store(0, Ordering::SeqCst);
    }
}

pub struct FakeGateway {
    addr: std::net::SocketAddr,
    pub counts: GatewayCounts,
}

impl FakeGateway {
    pub async fn start() -> Self {
        let counts = GatewayCounts::default();
        let router = Router::new()
            .route("/embeddings", post(embeddings))
            .route("/chat/completions", post(completions))
            .with_state(counts.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind the fake gateway");
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Self { addr, counts }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// Deterministic, content-addressed embedding: `unit_vector(rng_seeded_by(sha256(text)))`.
///
/// **This is what makes the isolation test sharp rather than merely green.** The same string always
/// yields the same vector, so an exact-match search scores ~1.0 — far above the harness's 0.5 floor.
/// Two unrelated strings land near-orthogonal (~0.0) in 1536 dimensions. So when tenant B searches
/// for tenant A's text and gets nothing, that emptiness can *only* be the tenant filter. With a
/// random or constant embedder it would be indistinguishable from "nothing cleared the threshold",
/// and the denial assertion would pass for the wrong reason.
pub fn fake_embedding(text: &str) -> Vec<f32> {
    use sha2::{Digest, Sha256};

    // A tiny SplitMix64 seeded from the text's digest — reproducible without an rng dependency.
    let digest = Sha256::digest(text.as_bytes());
    let mut state = u64::from_le_bytes(digest[..8].try_into().unwrap());
    let mut next = || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // Map into [-1, 1).
        (z as i64 as f64 / i64::MAX as f64) as f32
    };

    let mut v: Vec<f32> = (0..EMBEDDING_DIM).map(|_| next()).collect();
    // Normalise: the collection is cosine, and a unit vector makes an exact match score exactly 1.0.
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

async fn embeddings(State(counts): State<GatewayCounts>, Json(body): Json<Value>) -> Json<Value> {
    counts.embeddings.fetch_add(1, Ordering::SeqCst);

    let inputs: Vec<String> = body["input"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // `index` is load-bearing: the client sorts on it because a real gateway does not guarantee
    // response order, and a mismatch would bind a chunk's text to another chunk's vector.
    let data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(i, text)| json!({"index": i, "embedding": fake_embedding(text)}))
        .collect();

    Json(json!({ "data": data }))
}

/// The canned answer. Deliberately not a plausible support reply — if this string ever reaches a
/// user, the test config leaked into a real run and it should be obvious at a glance.
pub const FAKE_ANSWER: &str = "FAKE_LLM_ANSWER for testing only";

async fn completions(
    State(counts): State<GatewayCounts>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    counts.completions.fetch_add(1, Ordering::SeqCst);

    if body["stream"].as_bool().unwrap_or(false) {
        // The SSE shape /ask/stream consumes: content deltas, then [DONE].
        let sse = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":{"content": FAKE_ANSWER}}]}),
            json!({"choices":[{"delta":{}, "finish_reason":"stop"}]}),
        );
        return ([("content-type", "text/event-stream")], sse).into_response();
    }

    Json(json!({
        "choices": [{"message": {"role": "assistant", "content": FAKE_ANSWER}}]
    }))
    .into_response()
}
