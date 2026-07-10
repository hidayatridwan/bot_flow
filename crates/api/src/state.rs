use std::sync::{Arc, Mutex};

use crate::llm::LlmClient;
use fastembed::TextEmbedding;
use lapin::Channel;
use qdrant_client::Qdrant;
use s3::Bucket;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub qdrant: Arc<Qdrant>,
    pub embedder: Arc<Mutex<TextEmbedding>>,
    pub llm: LlmClient,
    pub s3: Box<Bucket>, // cheap to clone (just config/creds), fine per-request
    /// Bound to the endpoint the *client* reaches. Presigning only — see storage::build_public_bucket.
    pub s3_public: Box<Bucket>,
    pub presign_ttl_secs: u32,
    pub amqp: Channel,   // lapin Channel is cheaply cloneable (Arc inside)
    pub rag_score_threshold: f32,
    pub redis: redis::aio::ConnectionManager,
    pub rate_limit_per_minute: u64,
    pub admin_api_key: String,
}
