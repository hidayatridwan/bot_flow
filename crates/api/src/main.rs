mod auth;
mod config;
mod conversation;
mod db;
mod embedding;
mod error;
mod handlers;
mod llm;
mod queue;
mod rate_limit;
mod state;
mod storage;
mod upload;
use std::sync::{Arc, Mutex};
use tower_http::cors::{Any, CorsLayer};

use anyhow::Context;
use axum::{
    routing::{get, post},
    Router,
};
use qdrant_client::Qdrant;
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::llm::LlmClient;
use crate::state::AppState;
use axum::extract::DefaultBodyLimit;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = Config::from_env()?;

    // Admin pool (superuser) — ONLY to run migrations, including creating app_user (0005).
    let admin_db = PgPoolOptions::new()
        .max_connections(1)
        .connect(&config.database_url)
        .await
        .context("failed to connect to Postgres (admin)")?;
    sqlx::migrate!()
        .run(&admin_db)
        .await
        .context("failed to run database migrations")?;
    admin_db.close().await; // done with admin privileges
    tracing::info!("database migrations complete");

    // Runtime pool — non-superuser app_user, so RLS applies to every query.
    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.app_database_url)
        .await
        .context("failed to connect to Postgres (app_user)")?;
    tracing::info!("connected to Postgres as app_user");

    let qdrant = Qdrant::from_url(&config.qdrant_url)
        .build()
        .context("failed to build Qdrant client")?;
    tracing::info!("Qdrant client ready");

    handlers::ensure_collection(&qdrant).await?;

    tracing::info!("loading embedding model (the first download may take a while)...");

    let redis = redis::Client::open(config.redis_url.clone())
        .context("invalid REDIS_URL")?
        .get_connection_manager()
        .await
        .context("failed to connect to Redis")?;
    tracing::info!("Redis connection ready");

    storage::ensure_bucket(&config).await?;
    let s3 = storage::build_bucket(&config)?;
    let s3_public = storage::build_public_bucket(&config)?;
    tracing::info!(
        "S3 bucket ready (presigning against {})",
        config.s3_public_endpoint
    );

    // Keep `amqp_conn` bound for the whole program: dropping the Connection closes the
    // Channel. `main` runs forever (axum::serve), so this local lives long enough.
    let amqp_conn = lapin::Connection::connect(
        &config.rabbitmq_url,
        lapin::ConnectionProperties::default()
            .with_executor(tokio_executor_trait::Tokio::current())
            .with_reactor(tokio_reactor_trait::Tokio),
    )
    .await
    .context("failed to connect to RabbitMQ")?;
    let amqp = amqp_conn.create_channel().await?;
    tracing::info!("RabbitMQ channel ready");

    let embedder = tokio::task::spawn_blocking(embedding::init_embedder)
        .await
        .context("model loading task panicked")??;
    tracing::info!("embedding model ready");

    let state = AppState {
        db,
        qdrant: Arc::new(qdrant),
        embedder: Arc::new(Mutex::new(embedder)),
        llm: LlmClient::new(
            config.llm_base_url.clone(),
            config.llm_api_key.clone(),
            config.llm_model.clone(),
        ),
        s3,
        s3_public,
        presign_ttl_secs: config.presign_ttl_secs,
        amqp,
        rag_score_threshold: config.rag_score_threshold,
        redis,
        rate_limit_per_minute: config.rate_limit_per_minute,
        admin_api_key: config.admin_api_key.clone(),
    };

    // Browsers block cross-origin calls without CORS. We allow any origin here because the REAL
    // authorization is server-side: the publishable key + its per-tenant allowed_origins check.
    // No cookies are used (Bearer token), so a permissive policy is safe.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(handlers::health))
        .route("/ingest", post(handlers::ingest))
        .route("/search", post(handlers::search))
        .route("/ask", post(handlers::ask))
        .route(
            "/documents",
            get(handlers::list_documents)
                // DEPRECATED: proxies bytes through the API. Kept until clients move to
                // /documents/upload-url, then deleted along with queue.rs.
                .post(handlers::upload_document)
                .layer(DefaultBodyLimit::max(25 * 1024 * 1024)),
        )
        .route("/documents/upload-url", post(handlers::create_upload_url))
        .route(
            "/documents/{document_id}/upload-url",
            post(handlers::refresh_upload_url),
        )
        .route("/ask/stream", post(handlers::ask_stream))
        .route("/admin/tenants", post(handlers::create_tenant))
        .route("/admin/tenants/{tenant_id}/keys", post(handlers::mint_key))
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr)
        .await
        .context("failed to bind listener")?;
    tracing::info!("listening on {}", config.bind_addr);

    axum::serve(listener, app).await.context("server error")?;
    Ok(())
}
