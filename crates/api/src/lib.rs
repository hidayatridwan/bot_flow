//! The API's composition root.
//!
//! `main.rs` is a thin shell over this: everything that decides *what the server is* lives here, so
//! the integration suite in `tests/` can build the same `Router` over the same `AppState` that
//! production runs. The alternative — driving a spawned binary over a socket — cannot reach the
//! extractors, and the extractors are where the interesting bugs live (`Actor::from_request_parts`
//! needs a database, which is why the auth matrix has never been unit-testable).
//!
//! Only five items are public, and `main` is the first consumer of each. Every handler, gate and
//! query stays crate-private: the tests reach them **through HTTP**, which is the point of having
//! them in `tests/` rather than in-crate.

mod accounts;
mod auth;
mod conversation;
mod db;
mod erasure;
mod error;
mod handlers;
mod llm;
mod queue;
mod rate_limit;
mod storage;
mod upload;
mod widget;

pub mod config;
pub mod state;

/// Re-exported so the integration harness names the same collection the code does. It used a
/// string literal until phase 11, and kept using the pre-phase-10 name after the rename — teardown
/// deleted from a collection that no longer existed, silently, for a whole phase.
pub use common::COLLECTION;

use std::sync::Arc;

use anyhow::Context;
use axum::{
    extract::DefaultBodyLimit,
    routing::{delete, get, patch, post},
    Router,
};
use qdrant_client::Qdrant;
use sqlx::postgres::PgPoolOptions;
use tower_http::cors::{Any, CorsLayer};

use crate::config::Config;
use crate::llm::LlmClient;
use crate::state::AppState;
use common::embedding::EmbeddingClient;

/// Run migrations on the **admin** (superuser) pool, then close it.
///
/// The close is not tidiness: migration 0005 creates `app_user` precisely so the runtime can be a
/// non-superuser and RLS can apply to it (isolation layer 3). Leaving a superuser pool open is
/// leaving a way to bypass that, one well-meaning refactor away.
pub async fn run_migrations(database_url: &str) -> anyhow::Result<()> {
    let admin_db = PgPoolOptions::new()
        .max_connections(1)
        .connect(database_url)
        .await
        .context("failed to connect to Postgres (admin)")?;
    sqlx::migrate!()
        .run(&admin_db)
        .await
        .context("failed to run database migrations")?;
    admin_db.close().await; // done with admin privileges
    tracing::info!("database migrations complete");
    Ok(())
}

/// Connect every dependency and assemble the `AppState`.
///
/// **The returned `lapin::Connection` must be held for as long as the state is used.** Dropping it
/// closes the `Channel` inside `AppState`, and the only symptom is `/health` reporting rabbitmq
/// down — nothing else fails loudly. Returning it rather than binding it in `main` makes that a
/// type-level fact: `let (state, _) = build_state(..)` is visibly wrong, where a stray local was
/// only wrong if you had read the comment.
pub async fn build_state(config: &Config) -> anyhow::Result<(AppState, lapin::Connection)> {
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

    // Startup, not routing: the tenant_id keyword index must exist before any ingest, because
    // adding it after data exists does not retroactively restructure Qdrant's HNSW graph.
    handlers::ensure_collection(&qdrant).await?;

    let redis = redis::Client::open(config.redis_url.clone())
        .context("invalid REDIS_URL")?
        .get_connection_manager()
        .await
        .context("failed to connect to Redis")?;
    tracing::info!("Redis connection ready");

    storage::ensure_bucket(config).await?;
    let s3 = storage::build_bucket(config)?;
    let s3_public = storage::build_public_bucket(config)?;
    tracing::info!(
        "S3 bucket ready (presigning against {})",
        config.s3_public_endpoint
    );

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

    let state = AppState {
        db,
        qdrant: Arc::new(qdrant),
        // No connection is made here. A bad key or an unreachable endpoint surfaces on the first
        // question, not at boot.
        embedder: EmbeddingClient::new(
            config.embedding_base_url.clone(),
            config.embedding_api_key.clone(),
            config.embedding_model.clone(),
        ),
        llm: LlmClient::new(
            config.llm_base_url.clone(),
            config.llm_api_key.clone(),
            config.llm_model.clone(),
        ),
        s3,
        s3_public,
        presign_ttl_secs: config.presign_ttl_secs,
        max_upload_bytes: config.max_upload_bytes,
        amqp,
        rag_score_threshold: config.rag_score_threshold,
        redis,
        rate_limit_per_minute: config.rate_limit_per_minute,
        admin_api_key: config.admin_api_key.clone(),
        session_ttl_secs: config.session_ttl_secs,
    };

    Ok((state, amqp_conn))
}

/// The whole HTTP surface. Every route the binary serves, and every route the tests drive.
pub fn app(state: AppState) -> Router {
    // Browsers block cross-origin calls without CORS. We allow any origin here because the REAL
    // authorization is server-side: the publishable key + its per-tenant allowed_origins check.
    // No cookies are used (Bearer token), so a permissive policy is safe.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/health", get(handlers::health))
        // Public and unauthenticated, like /health: it is the file a visitor's browser loads before
        // it holds any credential. Served from the binary; cache-revalidated (phase 7).
        .route("/widget.js", get(widget::serve))
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
        // Erase a document across Postgres, Qdrant and MinIO (phase 8). Management-gated, like the
        // rest of /documents — never a `pk_`.
        .route(
            "/documents/{document_id}",
            delete(handlers::delete_document),
        )
        .route("/ask/stream", post(handlers::ask_stream))
        // Self-serve tenant accounts. /register and /login are public (and rate limited); the rest
        // require a session. The web BFF turns a session into an httpOnly cookie on its own origin —
        // sessions stay bearer tokens here, so the permissive-CORS/no-cookie reasoning is untouched.
        .route("/auth/register", post(accounts::register))
        .route("/auth/login", post(accounts::login))
        .route("/auth/logout", post(accounts::logout))
        .route("/auth/me", get(accounts::me))
        .route(
            "/auth/keys",
            get(accounts::list_keys).post(accounts::create_key),
        )
        .route(
            "/auth/keys/{key_hash}",
            patch(accounts::update_key).delete(accounts::revoke_key),
        )
        .route("/admin/tenants", post(handlers::create_tenant))
        .route("/admin/tenants/{tenant_id}/keys", post(handlers::mint_key))
        // Ends a tenant: vectors, objects, rows. Admin-gated like tenant creation — these are the
        // operations that make and unmake the tenancy registry (phase 12).
        .route(
            "/admin/tenants/{tenant_id}",
            delete(handlers::delete_tenant),
        )
        .layer(cors)
        .with_state(state)
}
