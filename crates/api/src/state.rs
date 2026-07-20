use std::sync::Arc;

use crate::llm::LlmClient;
use common::embedding::EmbeddingClient;
use lapin::Channel;
use qdrant_client::Qdrant;
use s3::Bucket;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub qdrant: Arc<Qdrant>,
    pub embedder: EmbeddingClient, // cheap to clone (reqwest::Client is Arc inside)
    pub llm: LlmClient,
    pub s3: Box<Bucket>, // cheap to clone (just config/creds), fine per-request
    /// Bound to the endpoint the *client* reaches. Presigning only — see storage::build_public_bucket.
    pub s3_public: Box<Bucket>,
    pub presign_ttl_secs: u32,
    pub max_upload_bytes: usize,
    pub amqp: Channel, // lapin Channel is cheaply cloneable (Arc inside)
    pub rag_score_threshold: f32,
    pub redis: redis::aio::ConnectionManager,
    pub rate_limit_per_minute: u64,
    pub admin_api_key: String,
    pub metrics_token: Option<String>,
    /// One field, not eleven: `AppState` is already large enough.
    pub metrics: std::sync::Arc<crate::metrics::Metrics>,
    /// The connection, alongside the channel, because reading queue depth needs a **throwaway**
    /// channel. A passive declare of a queue that does not exist is a channel-level NOT_FOUND and
    /// the broker closes the channel — so doing it on `amqp` would kill the publishing channel, and
    /// the only symptom anywhere would be /health reporting rabbitmq down. That is the
    /// `lapin::Connection` trap one layer up.
    pub amqp_conn: std::sync::Arc<lapin::Connection>,
    pub session_ttl_secs: i64,
    /// Outbound mail. Built at boot so a bad `SMTP_URL` fails the process rather than the first
    /// locked-out user's reset request.
    pub mailer: crate::mail::Mailer,
    /// The web app's origin, for building reset links. See `Config::app_base_url`.
    pub app_base_url: String,
}
