use anyhow::Context;

pub struct Config {
    pub database_url: String,
    pub qdrant_url: String,
    pub bind_addr: String,
    pub llm_base_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    /// Defaults to `llm_base_url` — usually the same gateway. The *key* never defaults.
    pub embedding_base_url: String,
    pub embedding_api_key: String,
    pub embedding_model: String,
    pub s3_endpoint: String,
    /// The endpoint clients reach MinIO on. Signed into presigned URLs; must match the Host
    /// the client actually connects to. Defaults to `s3_endpoint` (correct for local dev).
    pub s3_public_endpoint: String,
    pub presign_ttl_secs: u32,
    /// The same cap the worker enforces on an uploaded object. The API needs it too as of phase 11:
    /// `POST /ingest` receives the bytes in the request body, so unlike a presigned PUT it *can* be
    /// bounded before anything is stored. Invariant 11's "there is no earlier" is about signatures,
    /// not about wishes — where an earlier exists, use it.
    pub max_upload_bytes: usize,
    pub s3_bucket: String,
    pub s3_access_key: String,
    pub s3_secret_key: String,
    pub s3_region: String,
    pub rabbitmq_url: String,
    pub rag_score_threshold: f32,
    pub redis_url: String,
    pub rate_limit_per_minute: u64,
    pub app_database_url: String,
    pub admin_api_key: String,
    /// Guards `GET /metrics`. **Deliberately not `ADMIN_API_KEY`**: a scrape config is a long-lived,
    /// widely-readable artifact owned by whoever runs monitoring, and the admin key can now *erase a
    /// tenant* (phase 12). Trading "monitoring can read counters" for "monitoring can delete
    /// customers" is not a trade.
    ///
    /// `None` means the route is **not registered at all** — a 404, not a 401. Absent config,
    /// absent surface: an endpoint that refuses still confirms it exists.
    pub metrics_token: Option<String>,
    /// How long a login session stays valid, in seconds. Bound into the sessions.expires_at
    /// computed at mint time; there is no refresh — an expired token means log in again.
    pub session_ttl_secs: i64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?,
            qdrant_url: std::env::var("QDRANT_URL").context("QDRANT_URL is not set")?,
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string()),
            llm_base_url: std::env::var("LLM_BASE_URL").context("LLM_BASE_URL is not set")?,
            llm_api_key: std::env::var("LLM_API_KEY").context("LLM_API_KEY is not set")?,
            llm_model: std::env::var("LLM_MODEL")
                .unwrap_or_else(|_| "gemini/gemini-2.5-flash-lite".to_string()),
            embedding_base_url: std::env::var("EMBEDDING_BASE_URL")
                .or_else(|_| std::env::var("LLM_BASE_URL"))
                .context("neither EMBEDDING_BASE_URL nor LLM_BASE_URL is set")?,
            // Required, and deliberately not falling back to LLM_API_KEY: the two are separate
            // credentials even on one gateway, and a silent fallback would send the chat key to
            // the embeddings endpoint and blame the endpoint for the 401.
            embedding_api_key: std::env::var("EMBEDDING_API_KEY")
                .context("EMBEDDING_API_KEY is not set")?,
            // Changing this invalidates every stored vector — it is a migration, not a config edit.
            embedding_model: std::env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "text-embedding-3-small".to_string()),
            s3_endpoint: std::env::var("S3_ENDPOINT").context("S3_ENDPOINT is not set")?,
            s3_public_endpoint: std::env::var("S3_PUBLIC_ENDPOINT")
                .or_else(|_| std::env::var("S3_ENDPOINT"))
                .context("S3_ENDPOINT is not set")?,
            // 15 minutes: long enough for a slow upload, short enough that a leaked URL rots.
            max_upload_bytes: std::env::var("MAX_UPLOAD_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(25 * 1024 * 1024),
            presign_ttl_secs: std::env::var("PRESIGN_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(900),
            s3_bucket: std::env::var("S3_BUCKET").unwrap_or_else(|_| "documents".to_string()),
            s3_access_key: std::env::var("S3_ACCESS_KEY").context("S3_ACCESS_KEY is not set")?,
            s3_secret_key: std::env::var("S3_SECRET_KEY").context("S3_SECRET_KEY is not set")?,
            s3_region: std::env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
            rabbitmq_url: std::env::var("RABBITMQ_URL").context("RABBITMQ_URL is not set")?,
            // Cosine similarity floor for a retrieved chunk to count as "relevant".
            //
            // 0.25, measured on the phase-10 bench, and it is the ONLY value now. It used to be three: 0.70 compiled (an E5-era
            // number that made the bot refuse everything with text-embedding-3-small), 0.35
            // recommended by README, and whatever was actually in `.env`. A floor that disagrees
            // with itself in three places is a floor nobody owns, and getting it wrong is silent —
            // refusing when nothing clears the floor is the designed behaviour (invariant 4), so a
            // system that knows nothing looks exactly like one that works.
            //
            // Chosen by sweeping it on the bench rather than by reasoning about it, and the
            // reasoning would have been wrong: README recommended 0.35, which costs recall@3
            // 1.000 -> 0.955 and recall@1 0.909 -> 0.886 — it silently drops real answers. 0.25 is
            // the highest floor that costs nothing (identical recall to 0.20, ~5% less context);
            // 0.30 already loses a question. Re-sweep with `cargo run -p eval` after any change to
            // the chunker or the embedding model, and read `POST /search`, which deliberately does
            // not apply this floor so you can see what sits just below it.
            rag_score_threshold: std::env::var("RAG_SCORE_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.25),
            redis_url: std::env::var("REDIS_URL").context("REDIS_URL is not set")?,
            rate_limit_per_minute: std::env::var("RATE_LIMIT_PER_MINUTE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60),
            app_database_url: std::env::var("APP_DATABASE_URL")
                .context("APP_DATABASE_URL is not set")?,
            metrics_token: std::env::var("METRICS_TOKEN")
                .ok()
                .filter(|t| !t.is_empty()),
            admin_api_key: std::env::var("ADMIN_API_KEY").context("ADMIN_API_KEY is not set")?,
            // 30 days. Long enough that a dashboard user isn't nagged; short enough that a
            // stolen token rots. Session rows outlive this until a login/logout touches them.
            session_ttl_secs: std::env::var("SESSION_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2_592_000),
        })
    }
}
