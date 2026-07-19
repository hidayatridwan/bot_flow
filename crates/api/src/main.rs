//! Thin shell over `api::{run_migrations, build_state, app}` — see `lib.rs` for why the composition
//! root lives there rather than here.

use anyhow::Context;
use api::config::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = Config::from_env()?;

    api::run_migrations(&config.database_url).await?;

    // `_amqp` is not unused: dropping the Connection closes the Channel inside the state, and the
    // only symptom is /health reporting rabbitmq down. It must outlive `axum::serve`.
    let (state, _amqp) = api::build_state(&config).await?;

    let listener = tokio::net::TcpListener::bind(&config.bind_addr)
        .await
        .context("failed to bind listener")?;
    tracing::info!("listening on {}", config.bind_addr);

    axum::serve(listener, api::app(state))
        .await
        .context("server error")?;
    Ok(())
}
