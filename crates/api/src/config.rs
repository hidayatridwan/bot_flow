use anyhow::Context;

pub struct Config {
    pub database_url: String,
    pub qdrant_url: String,
    pub bind_addr: String,
    pub llm_base_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub s3_endpoint: String,
    /// The endpoint clients reach MinIO on. Signed into presigned URLs; must match the Host
    /// the client actually connects to. Defaults to `s3_endpoint` (correct for local dev).
    pub s3_public_endpoint: String,
    pub presign_ttl_secs: u32,
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
            s3_endpoint: std::env::var("S3_ENDPOINT").context("S3_ENDPOINT is not set")?,
            s3_public_endpoint: std::env::var("S3_PUBLIC_ENDPOINT")
                .or_else(|_| std::env::var("S3_ENDPOINT"))
                .context("S3_ENDPOINT is not set")?,
            // 15 minutes: long enough for a slow upload, short enough that a leaked URL rots.
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
            // MultilingualE5Small scores a chunk that verbatim answers the question around
            // 0.78-0.86, so anything at or above 0.80 rejects correct passages. Tunable
            // without recompiling — watch the logged retrieval scores and adjust.
            rag_score_threshold: std::env::var("RAG_SCORE_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.70),
            redis_url: std::env::var("REDIS_URL").context("REDIS_URL is not set")?,
            rate_limit_per_minute: std::env::var("RATE_LIMIT_PER_MINUTE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60),
            app_database_url: std::env::var("APP_DATABASE_URL")
                .context("APP_DATABASE_URL is not set")?,
            admin_api_key: std::env::var("ADMIN_API_KEY").context("ADMIN_API_KEY is not set")?,
        })
    }
}
